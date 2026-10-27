//! Pure Window-local model of the agent access inventory (ADR 0014, Plan 0053
//! PR 5): the **Agent access** list, its revoke confirmations, **Revoke all
//! access** with exact counts, the Explorer marks, and the waiting indicator.
//!
//! Everything here is display state derived from the daemon's `ListAccess`
//! inventory. It never grants anything: the daemon decides every revoke, and
//! a stale view can at worst name an entry that no longer exists. Agent
//! names, paths, and workspace names are untrusted display data, so they are
//! always shown escaped and bounded, and an agent name is never an identity.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use splinterm_automation_client::{access_entry_id, consent_scope_name};
use splinterm_core::{DojoId, LairId, SplintId};
use splinterm_protocol::{
    AccessEntry, AccessEntryKey, AccessEntryKind, AccessEntryState, AccessIdentityKind,
    AccessPolicyResource, AccessRevocationCounts, AccessRevokeTarget, AutomationScope,
    RememberedStoreStatus,
};

use super::{action_menu::TerminationDecision, text_edit::BoundedTextEditor};

/// Entries one Window reads at most: eight inventory pages.
pub const MAX_WINDOW_ACCESS_ENTRIES: usize = 512;
pub(crate) const AGENT_ACCESS_PAGE_ITEMS: usize = 7;
const MAX_QUERY_BYTES: usize = 256;
const MAX_QUERY_SCALARS: usize = 128;
/// Scalars of one agent, workspace, or rule name shown in a row.
const MAX_NAME_SCALARS: usize = 48;
/// Scalars of an executable path shown in the footer or a prompt.
const MAX_PATH_SCALARS: usize = 160;
const MAX_NOTICE_SCALARS: usize = 240;

/// One read of the daemon-wide access inventory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessSnapshot {
    /// The daemon's `authorization_revision` when the read finished.
    pub revision: u64,
    pub remembered_store: RememberedStoreStatus,
    /// Every entry in inventory order, at most [`MAX_WINDOW_ACCESS_ENTRIES`].
    pub entries: Vec<AccessEntry>,
    /// The inventory held more entries than this Window reads.
    pub truncated: bool,
}

impl AccessSnapshot {
    #[must_use]
    pub fn new(revision: u64, remembered_store: RememberedStoreStatus) -> Self {
        Self {
            revision,
            remembered_store,
            entries: Vec::new(),
            truncated: false,
        }
    }

    /// The entry under `key`, if this read lists it.
    #[must_use]
    pub fn entry(&self, key: &AccessEntryKey) -> Option<&AccessEntry> {
        self.entries.iter().find(|entry| &entry.key == key)
    }

    /// Requests waiting for the person's answer, in every workspace.
    #[must_use]
    pub fn waiting(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.kind == AccessEntryKind::Pending)
            .count()
    }

    /// Whether **Revoke all access** has anything to do, as far as this read
    /// shows: a consent grant that still authorizes, or a pending request.
    #[must_use]
    pub fn revoke_all_has_targets(&self) -> bool {
        self.entries.iter().any(|entry| {
            entry.kind == AccessEntryKind::Pending
                || (entry.revocable && entry.kind != AccessEntryKind::PersistentPolicy)
        })
    }
}

/// What the Window asks the topology manager to do with access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowAccessCommand {
    /// Read the inventory now, as when the list opens.
    Refresh,
    /// Revoke one grant by the ID kind the inventory listed it under.
    RevokeEntry { target: AccessRevokeTarget },
    /// Count what **Revoke all access** would revoke (a dry run).
    CountRevokeAll,
    /// Revoke everything, but only if a fresh count still equals `expected`,
    /// the counts the person confirmed.
    RevokeAll { expected: AccessRevocationCounts },
}

/// What the topology manager tells the Window about access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowAccessUpdate {
    Snapshot(AccessSnapshot),
    /// The inventory cannot be read from this Window, and why.
    Unavailable(String),
    /// Exact counts for a **Revoke all access** confirmation. `recounted`
    /// says the person confirmed other numbers, so nothing was revoked and
    /// these must be confirmed again.
    RevokeAllCounts {
        counts: AccessRevocationCounts,
        recounted: bool,
    },
    /// The outcome of a revoke, for the person. `failed` is set when it did
    /// not happen, or a remembered grant could not be removed durably; `all`
    /// when it was **Revoke all access**; `entry` names the one entry a
    /// per-entry revoke answered.
    Revoked {
        message: String,
        failed: bool,
        all: bool,
        entry: Option<AccessEntryKey>,
    },
}

/// Whether a confirmed **Revoke all access** may proceed: only when a fresh
/// dry run counts exactly what the person confirmed. Anything else is shown
/// again with the new counts, so the person never confirms numbers that are
/// no longer true.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RevokeAllCheck {
    Proceed,
    Recount(AccessRevocationCounts),
}

#[must_use]
pub fn check_revoke_all(
    expected: AccessRevocationCounts,
    current: AccessRevocationCounts,
) -> RevokeAllCheck {
    if expected == current {
        RevokeAllCheck::Proceed
    } else {
        RevokeAllCheck::Recount(current)
    }
}

/// Whether a count has nothing to revoke.
#[must_use]
pub const fn counts_are_empty(counts: AccessRevocationCounts) -> bool {
    counts.once == 0
        && counts.workspace_session == 0
        && counts.remembered == 0
        && counts.pending_requests == 0
}

fn plural(count: u32, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

/// The person-facing counts of **Revoke all access**, before (`done` false)
/// or after it ran.
#[must_use]
pub fn revoke_all_summary(counts: AccessRevocationCounts, done: bool) -> String {
    format!(
        "{} {}, and {} {}, {}, and {}. Policy rules are not touched.",
        if done { "Removed" } else { "This removes" },
        plural(counts.remembered, "remembered grant", "remembered grants"),
        if done { "ended" } else { "ends" },
        plural(counts.once, "just-this-once grant", "just-this-once grants"),
        plural(
            counts.workspace_session,
            "while-open grant",
            "while-open grants"
        ),
        plural(
            counts.pending_requests,
            "pending request",
            "pending requests"
        ),
    )
}

/// Whether a character of untrusted text must never reach trusted UI as is:
/// controls, bidirectional and invisible formatting (zero-width spaces and
/// joiners, word joiner, invisible operators, isolates), the line and
/// paragraph separators, the byte-order mark, and the other characters that
/// draw nothing in common fonts (soft hyphen, Hangul fillers, Mongolian
/// selectors, interlinear annotation marks, musical formatting, and tag
/// characters, which could hide text after a name). Emoji variation selectors
/// (U+FE00–FE0F) stay, so emoji in names draw as emoji. The consent prompt,
/// the **Agent access** list, and `splinterm access list` use this one set.
#[must_use]
pub fn unsafe_display_character(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{00ad}'
                | '\u{034f}'
                | '\u{061c}'
                | '\u{115f}'
                | '\u{1160}'
                | '\u{17b4}'
                | '\u{17b5}'
                | '\u{180b}'..='\u{180f}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2060}'..='\u{2069}'
                | '\u{3164}'
                | '\u{feff}'
                | '\u{ffa0}'
                | '\u{fff0}'..='\u{fffb}'
                | '\u{1bca0}'..='\u{1bca3}'
                | '\u{1d173}'..='\u{1d17a}'
                | '\u{e0000}'..='\u{e0fff}'
        )
}

/// Untrusted display text, escaped and bounded: every
/// [`unsafe_display_character`] is shown as `\u{…}`, so text can neither
/// break a line nor reorder or hide the text around it, and long text ends
/// in an ellipsis.
#[must_use]
pub fn shown(text: &str, maximum_scalars: usize) -> String {
    shown_escaping(text, maximum_scalars, unsafe_display_character)
}

/// A name or path inside a list row or the consent prompt: also escapes the
/// quotes and the separator the row puts around and between names, so a
/// name cannot fake another segment of the row. The prompt escapes names
/// and paths with this too, so a name reads the same in the prompt that
/// approves it and in the list later. Whitespace is not collapsed: tabs,
/// newlines, and the other control whitespace are escaped like any unsafe
/// character, and spaces stay as they are.
#[must_use]
pub fn shown_name(text: &str, maximum_scalars: usize) -> String {
    shown_escaping(text, maximum_scalars, |character| {
        unsafe_display_character(character) || matches!(character, '‘' | '’' | '·')
    })
}

/// [`shown_name`] for the consent prompt, which quotes names with ASCII `'`:
/// that quote is escaped too, so a name cannot close its quote early and
/// make its own text read as the prompt's fixed wording.
#[must_use]
pub fn shown_prompt_name(text: &str, maximum_scalars: usize) -> String {
    shown_escaping(text, maximum_scalars, |character| {
        unsafe_display_character(character) || matches!(character, '‘' | '’' | '·' | '\'')
    })
}

fn shown_escaping(text: &str, maximum_scalars: usize, escape: impl Fn(char) -> bool) -> String {
    let mut output = String::with_capacity(text.len().min(maximum_scalars * 4));
    let mut scalars = 0_usize;
    for character in text.chars() {
        let piece = if escape(character) {
            format!("\\u{{{:04x}}}", u32::from(character))
        } else {
            character.to_string()
        };
        let width = piece.chars().count();
        if scalars.saturating_add(width) > maximum_scalars {
            output.push('…');
            return output;
        }
        scalars = scalars.saturating_add(width);
        output.push_str(&piece);
    }
    output
}

fn short_id(id: &impl ToString) -> String {
    id.to_string().chars().take(8).collect()
}

/// `YYYY-MM-DD HH:MM UTC`, from Unix seconds.
fn utc(seconds: u64) -> String {
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX / 2);
    let minutes = seconds % 86_400 / 60;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        minutes / 60,
        minutes % 60
    )
}

/// The agent as a row names it: its reported name in quotes (display data,
/// never an identity), or else its executable's file name.
fn agent_text(entry: &AccessEntry) -> String {
    if let Some(name) = &entry.identity.display_name {
        return format!("‘{}’", shown_name(name, MAX_NAME_SCALARS));
    }
    let path = &entry.identity.executable_path;
    let name = path.rsplit('/').next().filter(|name| !name.is_empty());
    shown_name(name.unwrap_or(path), MAX_NAME_SCALARS)
}

fn access_text(entry: &AccessEntry) -> String {
    if let Some(level) = entry.level {
        return level.label().to_owned();
    }
    let scopes: Vec<_> = entry
        .scopes
        .iter()
        .filter_map(|scope| consent_scope_name(*scope))
        .collect();
    format!("Custom ({})", scopes.join(", "))
}

fn workspace_text(entry: &AccessEntry) -> String {
    let Some(workspace) = &entry.workspace else {
        return "no workspace".to_owned();
    };
    let short = short_id(&workspace.lair_id);
    match &workspace.name {
        Some(name) => format!("‘{}’ ({short})", shown_name(name, MAX_NAME_SCALARS)),
        None => format!("a missing workspace ({short})"),
    }
}

const fn identity_kind_text(kind: AccessIdentityKind) -> &'static str {
    match kind {
        AccessIdentityKind::Process => "one process",
        AccessIdentityKind::ExactBuild => "this exact build",
        AccessIdentityKind::ProtectedInstall => "protected install",
        AccessIdentityKind::NixProfile => "Nix profile",
        AccessIdentityKind::PolicyDigest => "exact digest in policy.json",
    }
}

fn policy_resources_text(resources: &[AccessPolicyResource]) -> String {
    resources
        .iter()
        .map(|resource| match resource {
            AccessPolicyResource::Daemon => "the daemon".to_owned(),
            AccessPolicyResource::Lair { lair_id } => format!("Lair {}", short_id(lair_id)),
            AccessPolicyResource::Dojo { dojo_id } => format!("Dojo {}", short_id(dojo_id)),
            AccessPolicyResource::Splint { splint_id } => {
                format!("Splint {}", short_id(splint_id))
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn rule_list(rules: &[String]) -> String {
    rules
        .iter()
        .map(|rule| format!("‘{}’", shown_name(rule, MAX_NAME_SCALARS)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The short tag a row starts with. A tag is a word, never only a colour.
const fn category(entry: &AccessEntry) -> &'static str {
    match entry.kind {
        AccessEntryKind::Remembered => "ALWAYS",
        AccessEntryKind::WorkspaceSession => "OPEN",
        AccessEntryKind::Once => "ONCE",
        AccessEntryKind::Pending => "WAITING",
        AccessEntryKind::PersistentPolicy => "POLICY",
    }
}

fn title(entry: &AccessEntry) -> String {
    let agent = agent_text(entry);
    let access = access_text(entry);
    let workspace = workspace_text(entry);
    match entry.kind {
        AccessEntryKind::Remembered => format!("{agent} · {access} · always for {workspace}"),
        AccessEntryKind::WorkspaceSession => {
            format!("{agent} · {access} · while {workspace} is open")
        }
        AccessEntryKind::Once => match entry.splint_id {
            Some(splint_id) if entry.workspace.is_some() => format!(
                "{agent} · {access} · once for Splint {} in {workspace}",
                short_id(&splint_id)
            ),
            Some(splint_id) => format!(
                "{agent} · {access} · once for Splint {}",
                short_id(&splint_id)
            ),
            None => format!("{agent} · {access} · once for {workspace}"),
        },
        AccessEntryKind::Pending => format!("{agent} asks for {access} on {workspace}"),
        AccessEntryKind::PersistentPolicy => {
            let rule = match &entry.key {
                AccessEntryKey::Policy { rule_id } => shown_name(rule_id, MAX_NAME_SCALARS),
                _ => String::new(),
            };
            format!(
                "{agent} · policy rule ‘{rule}’ · {}",
                policy_resources_text(&entry.policy_resources)
            )
        }
    }
}

fn status(entry: &AccessEntry) -> String {
    match (entry.kind, entry.state) {
        (AccessEntryKind::Remembered, AccessEntryState::WorkspaceMissing) => {
            "workspace missing".to_owned()
        }
        (AccessEntryKind::Remembered, AccessEntryState::RemovalPending) => {
            "returns on restart".to_owned()
        }
        (AccessEntryKind::Pending, AccessEntryState::Prompting) => "prompt showing".to_owned(),
        (AccessEntryKind::Pending, _) => "queued".to_owned(),
        _ if entry.controllers > 0 => {
            format!("{} connected · in control", entry.connections.max(1))
        }
        _ if entry.connections > 0 => format!("{} connected", entry.connections),
        _ => "not connected".to_owned(),
    }
}

fn hint(entry: &AccessEntry) -> String {
    match (entry.kind, entry.state) {
        (AccessEntryKind::Pending, _) => {
            "Waiting for your answer in its prompt; Revoke all access also ends it".to_owned()
        }
        (AccessEntryKind::PersistentPolicy, _) => {
            "Read-only here: remove it from policy.json to revoke".to_owned()
        }
        (AccessEntryKind::Remembered, AccessEntryState::RemovalPending) => {
            "Revoked, but returns after a restart until the store is written".to_owned()
        }
        (AccessEntryKind::Remembered, AccessEntryState::WorkspaceMissing) => {
            "Authorizes nothing · Enter removes it (asks first)".to_owned()
        }
        (AccessEntryKind::Remembered, _) => "Enter revokes it (asks first)".to_owned(),
        _ => "Enter revokes it".to_owned(),
    }
}

fn detail(entry: &AccessEntry) -> String {
    let mut parts = vec![
        hint(entry),
        format!(
            "{} ({})",
            shown_name(&entry.identity.executable_path, MAX_PATH_SCALARS),
            identity_kind_text(entry.identity.kind)
        ),
    ];
    if let Some(pid) = entry.identity.pid {
        parts.push(format!("process {pid}"));
    }
    if entry.kind == AccessEntryKind::Remembered {
        parts.push(entry.last_used_at_unix_seconds.map_or_else(
            || "never used".to_owned(),
            |used| format!("last used {}", utc(used)),
        ));
    }
    if let Some(expires) = entry.expires_at_unix_seconds {
        parts.push(format!("ends {}", utc(expires)));
    }
    if entry.controllers > 0 {
        parts.push(plural(entry.controllers, "controller", "controllers"));
    }
    if !entry.policy_rules.is_empty() {
        parts.push(format!(
            "access continues under policy rule {}",
            rule_list(&entry.policy_rules)
        ));
    }
    parts.join(" · ")
}

/// What a revoke of `entry` names: a runtime grant by its grant ID, or a
/// remembered entry by its durable `ra_` ID, never by the runtime grant ID
/// its controllers carry. Pending requests and policy rules are never
/// revocable here.
#[must_use]
pub fn revoke_target(entry: &AccessEntry) -> Option<AccessRevokeTarget> {
    if !entry.revocable {
        return None;
    }
    match (&entry.key, entry.kind) {
        (
            AccessEntryKey::Grant { grant_id },
            AccessEntryKind::Once | AccessEntryKind::WorkspaceSession,
        ) => Some(AccessRevokeTarget::Grant {
            grant_id: *grant_id,
        }),
        (AccessEntryKey::Remembered { entry_id }, AccessEntryKind::Remembered) => {
            Some(AccessRevokeTarget::Remembered {
                entry_id: entry_id.clone(),
            })
        }
        _ => None,
    }
}

/// One row of the **Agent access** list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AccessRow {
    pub(crate) key: AccessEntryKey,
    pub(crate) category: &'static str,
    pub(crate) title: String,
    pub(crate) status: String,
    /// The row in one line, for a narrow palette.
    pub(crate) compact: String,
    /// The selected row's footer: what Enter does, the executable, and
    /// times.
    pub(crate) detail: String,
    pub(crate) revocable: bool,
    search: String,
}

impl AccessRow {
    fn new(entry: &AccessEntry) -> Self {
        let category = category(entry);
        let title = title(entry);
        let status = status(entry);
        let detail = detail(entry);
        let search = format!(
            "{category} {title} {status} {} {}",
            access_entry_id(&entry.key),
            entry.identity.executable_path
        )
        .to_lowercase();
        Self {
            key: entry.key.clone(),
            category,
            compact: format!("{category} · {title} — {status}"),
            title,
            status,
            detail,
            revocable: revoke_target(entry).is_some(),
            search,
        }
    }

    fn matches(&self, query: &[String]) -> bool {
        query
            .iter()
            .all(|token| self.search.contains(token.as_str()))
    }
}

/// What activating a list row asks the Window to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AccessActivation {
    /// Revoke a shorter grant at once.
    Revoke(AccessRevokeTarget),
    /// Ask first: an "Always" grant.
    Confirm(RevokeAccessUi),
}

/// The **Agent access** list, shown in the command palette.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentAccessUi {
    snapshot: Option<AccessSnapshot>,
    unavailable: Option<String>,
    all_rows: Vec<AccessRow>,
    rows: Vec<AccessRow>,
    editor: BoundedTextEditor,
    filtered_query: String,
    selected: usize,
    visible_start: usize,
    notice: Option<String>,
    /// Entries revoked from this list whose revoke the daemon has not
    /// answered yet, so a second press cannot send it twice.
    revoking: HashSet<AccessEntryKey>,
    /// The selected entry left the list in a newer read, or the confirmation
    /// the person was answering closed because its entry ended: the row
    /// now under the selection is not the one they chose, so Enter on it
    /// must not act until they choose again.
    selection_stale: bool,
}

impl AgentAccessUi {
    pub(crate) fn new(snapshot: Option<&AccessSnapshot>, unavailable: Option<&str>) -> Self {
        let mut ui = Self {
            snapshot: None,
            unavailable: unavailable.map(|reason| shown(reason, MAX_NOTICE_SCALARS)),
            all_rows: Vec::new(),
            rows: Vec::new(),
            editor: BoundedTextEditor::new(
                String::new(),
                MAX_QUERY_BYTES,
                MAX_QUERY_SCALARS,
                false,
            ),
            filtered_query: String::new(),
            selected: 0,
            visible_start: 0,
            notice: None,
            revoking: HashSet::new(),
            selection_stale: false,
        };
        if let Some(snapshot) = snapshot {
            ui.apply_snapshot(snapshot);
        }
        ui
    }

    /// Replaces the listed inventory, keeping the selection on the same
    /// entry when it is still listed.
    pub(crate) fn apply_snapshot(&mut self, snapshot: &AccessSnapshot) {
        let selected = self.selected_key().cloned();
        self.unavailable = None;
        self.all_rows = snapshot.entries.iter().map(AccessRow::new).collect();
        self.revoking
            .retain(|key| snapshot.entry(key).is_some_and(|entry| entry.revocable));
        self.snapshot = Some(snapshot.clone());
        if !self.refilter(selected.as_ref()) {
            self.selection_stale = true;
        }
    }

    pub(crate) fn set_unavailable(&mut self, reason: &str) {
        self.unavailable = Some(shown(reason, MAX_NOTICE_SCALARS));
    }

    pub(crate) fn set_notice(&mut self, notice: &str) {
        self.notice = Some(shown(notice, MAX_NOTICE_SCALARS));
    }

    /// A revoke the daemon answered: that entry may be revoked again if it
    /// failed and is still listed; other revokes stay in flight.
    pub(crate) fn finish_revoke(&mut self, key: Option<&AccessEntryKey>, message: &str) {
        if let Some(key) = key {
            self.revoking.remove(key);
        }
        self.set_notice(message);
    }

    /// The confirmation the person was answering closed because its entry
    /// ended: nothing acts until they choose a row again.
    pub(crate) fn mark_selection_stale(&mut self, notice: &str) {
        self.selection_stale = true;
        self.set_notice(notice);
    }

    /// A pointer release on row `index`, after a press on the row that
    /// showed `pressed`. Only a press and release on the same entry count,
    /// and they only select it, the selected row and a stale selection
    /// included. A click never revokes: after a revoke the selection moves
    /// to another row, and clicking that row to choose it must not revoke
    /// it too. Only Enter acts. Whether the click counted.
    pub(crate) fn release_click(&mut self, pressed: &AccessEntryKey, index: usize) -> bool {
        if self.rows.get(index).is_none_or(|row| &row.key != pressed) {
            return false;
        }
        self.select_row(index);
        true
    }

    /// The entry on row `index`, for a pointer press.
    pub(crate) fn row_key(&self, index: usize) -> Option<&AccessEntryKey> {
        self.rows.get(index).map(|row| &row.key)
    }

    pub(crate) fn rows(&self) -> &[AccessRow] {
        &self.rows
    }

    /// How many entries the list shows with no query.
    pub(crate) fn total(&self) -> usize {
        self.all_rows.len()
    }

    pub(crate) const fn selected_index(&self) -> usize {
        self.selected
    }

    pub(crate) const fn visible_start(&self) -> usize {
        self.visible_start
    }

    pub(crate) fn query(&self) -> &str {
        self.editor.text()
    }

    pub(crate) fn editor_mut(&mut self) -> &mut BoundedTextEditor {
        &mut self.editor
    }

    pub(crate) fn editor_changed(&mut self) {
        if self.editor.text() != self.filtered_query {
            let selected = self.selected_key().cloned();
            let _ = self.refilter(selected.as_ref());
            // Searching is choosing again.
            self.selection_stale = false;
        }
    }

    #[cfg(test)]
    pub(crate) fn append_text(&mut self, text: &str) -> bool {
        if !self.editor.insert(text) {
            return false;
        }
        self.editor_changed();
        true
    }

    pub(crate) fn clear_query(&mut self) -> bool {
        if self.editor.text().is_empty() {
            return false;
        }
        let _ = self.editor.select_all();
        let _ = self.editor.cut();
        self.editor_changed();
        true
    }

    fn selected_key(&self) -> Option<&AccessEntryKey> {
        self.rows.get(self.selected).map(|row| &row.key)
    }

    fn selected_entry(&self) -> Option<&AccessEntry> {
        let key = self.selected_key()?;
        self.snapshot.as_ref()?.entry(key)
    }

    pub(crate) fn move_selection(&mut self, delta: isize) -> bool {
        if self.rows.is_empty() || delta == 0 {
            return false;
        }
        let was_stale = std::mem::take(&mut self.selection_stale);
        let previous = self.selected;
        let magnitude = delta.unsigned_abs().min(self.rows.len() - 1);
        self.selected = if delta.is_negative() {
            self.selected.saturating_sub(magnitude)
        } else {
            self.selected
                .saturating_add(magnitude)
                .min(self.rows.len() - 1)
        };
        self.ensure_visible();
        let changed = self.selected != previous || was_stale;
        if changed {
            self.notice = None;
        }
        changed
    }

    pub(crate) fn select_first(&mut self) -> bool {
        let was_stale = std::mem::take(&mut self.selection_stale);
        let changed = self.selected != 0 || self.visible_start != 0 || was_stale;
        self.selected = 0;
        self.visible_start = 0;
        changed
    }

    pub(crate) fn select_last(&mut self) -> bool {
        let Some(last) = self.rows.len().checked_sub(1) else {
            return false;
        };
        let was_stale = std::mem::take(&mut self.selection_stale);
        let changed = self.selected != last || was_stale;
        self.selected = last;
        self.ensure_visible();
        changed
    }

    /// Selects the row at `index` in the filtered list; whether anything
    /// changed, including a stale selection becoming a fresh choice.
    fn select_row(&mut self, index: usize) -> bool {
        if index >= self.rows.len() {
            return false;
        }
        let was_stale = std::mem::take(&mut self.selection_stale);
        let changed = index != self.selected || was_stale;
        self.selected = index;
        self.notice = None;
        self.ensure_visible();
        changed
    }

    #[cfg(test)]
    pub(crate) fn select_index(&mut self, index: usize) -> bool {
        self.select_row(index)
    }

    /// What Enter does; a click only selects. Rows that cannot be revoked
    /// here explain why instead, and a stale selection does nothing until
    /// the person chooses again.
    pub(crate) fn activate(&mut self) -> Option<AccessActivation> {
        if self.selection_stale {
            self.set_notice("That access ended; choose again");
            return None;
        }
        let entry = self.selected_entry()?.clone();
        if self.revoking.contains(&entry.key) {
            self.set_notice("Revoking…");
            return None;
        }
        let Some(target) = revoke_target(&entry) else {
            self.set_notice(&hint(&entry));
            return None;
        };
        if entry.kind == AccessEntryKind::Remembered {
            return Some(AccessActivation::Confirm(RevokeAccessUi::entry(
                target, entry,
            )));
        }
        self.revoking.insert(entry.key.clone());
        self.set_notice("Revoking…");
        Some(AccessActivation::Revoke(target))
    }

    /// Records that a confirmed revoke of `target` was sent.
    pub(crate) fn begin_revoke(&mut self, target: &AccessRevokeTarget) {
        self.revoking.insert(target.key());
        self.set_notice("Revoking…");
    }

    /// What the palette's frame says before its entry count, where it
    /// stays visible: how many requests wait, then that the read stopped at
    /// this Window's bound. Empty when neither applies.
    pub(crate) fn status_parts(&self) -> Vec<String> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        let mut status = Vec::new();
        match snapshot.waiting() {
            0 => {}
            waiting => status.push(format!("{waiting} waiting")),
        }
        if snapshot.truncated {
            status.push(format!("first {MAX_WINDOW_ACCESS_ENTRIES} only"));
        }
        status
    }

    /// [`Self::status_parts`] joined, as the frame shows them when they fit.
    #[cfg(test)]
    pub(crate) fn status_label(&self) -> String {
        self.status_parts().join(" · ")
    }

    /// The text shown when no row is, or `None` when rows are listed.
    pub(crate) fn empty_label(&self) -> Option<String> {
        if !self.rows.is_empty() {
            return None;
        }
        Some(if let Some(reason) = &self.unavailable {
            format!("Agent access unavailable: {reason}")
        } else if self.snapshot.is_none() {
            "Loading agent access…".to_owned()
        } else if self.all_rows.is_empty() {
            "No agent has access to any workspace.".to_owned()
        } else {
            "No matching access".to_owned()
        })
    }

    /// The footer line: a notice, the remembered store's failure, or what
    /// the selected row is and what Enter does.
    pub(crate) fn footer(&self) -> String {
        if let Some(notice) = &self.notice {
            return notice.clone();
        }
        if let Some(reason) = &self.unavailable
            && self.snapshot.is_some()
        {
            return format!("Not current: {reason}");
        }
        if let Some(RememberedStoreStatus::Unavailable { reason }) = self
            .snapshot
            .as_ref()
            .map(|snapshot| &snapshot.remembered_store)
        {
            return format!(
                "Remembered access is unavailable: {}; “Always” grants authorize nothing until it is repaired",
                shown(reason, MAX_NOTICE_SCALARS)
            );
        }
        if let Some(row) = self.rows.get(self.selected) {
            return row.detail.clone();
        }
        if self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.truncated)
        {
            return "`splinterm access list` shows every entry".to_owned();
        }
        "Type to search · Esc close".to_owned()
    }

    /// Filters the rows; whether the kept entry is still listed.
    fn refilter(&mut self, keep: Option<&AccessEntryKey>) -> bool {
        let query = self.editor.text().to_owned();
        self.filtered_query.clone_from(&query);
        let tokens = query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect::<Vec<_>>();
        self.rows = self
            .all_rows
            .iter()
            .filter(|row| row.matches(&tokens))
            .cloned()
            .collect();
        let kept = keep.and_then(|key| self.rows.iter().position(|row| &row.key == key));
        self.selected = kept.unwrap_or(0);
        self.ensure_visible();
        keep.is_none() || kept.is_some()
    }

    fn ensure_visible(&mut self) {
        if self.rows.is_empty() {
            self.selected = 0;
            self.visible_start = 0;
            return;
        }
        self.selected = self.selected.min(self.rows.len() - 1);
        if self.selected < self.visible_start {
            self.visible_start = self.selected;
        } else if self.selected >= self.visible_start.saturating_add(AGENT_ACCESS_PAGE_ITEMS) {
            self.visible_start = self
                .selected
                .saturating_add(1)
                .saturating_sub(AGENT_ACCESS_PAGE_ITEMS);
        }
    }
}

/// What a revoke prompt asks, or tells.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AccessPrompt {
    /// Revoke one "Always" grant. `changed` says the entry changed after the
    /// person chose it, so it must be checked again.
    Entry {
        target: AccessRevokeTarget,
        entry: Box<AccessEntry>,
        changed: bool,
    },
    /// **Revoke all access** with exact counts. `recounted` says the person
    /// confirmed other numbers before.
    All {
        counts: AccessRevocationCounts,
        recounted: bool,
    },
    /// A result the person reads and closes.
    Notice { title: &'static str, body: String },
}

/// How a revoke prompt fits a newer inventory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromptReconcile {
    Unchanged,
    /// The entry changed: the prompt shows it again and is back on Cancel.
    Changed,
    /// The entry is gone: the prompt has nothing left to revoke.
    Gone,
}

/// A revoke confirmation in the Window's prompt. Cancel is preselected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RevokeAccessUi {
    prompt: AccessPrompt,
    decision: TerminationDecision,
}

impl RevokeAccessUi {
    pub(crate) fn entry(target: AccessRevokeTarget, entry: AccessEntry) -> Self {
        Self {
            prompt: AccessPrompt::Entry {
                target,
                entry: Box::new(entry),
                changed: false,
            },
            decision: TerminationDecision::Cancel,
        }
    }

    pub(crate) const fn all(counts: AccessRevocationCounts, recounted: bool) -> Self {
        Self {
            prompt: AccessPrompt::All { counts, recounted },
            decision: TerminationDecision::Cancel,
        }
    }

    pub(crate) fn notice(title: &'static str, body: &str) -> Self {
        Self {
            prompt: AccessPrompt::Notice {
                title,
                body: shown(body, MAX_NOTICE_SCALARS * 2),
            },
            decision: TerminationDecision::Cancel,
        }
    }

    pub(crate) const fn is_notice(&self) -> bool {
        matches!(self.prompt, AccessPrompt::Notice { .. })
    }

    pub(crate) const fn decision(&self) -> Option<TerminationDecision> {
        if self.is_notice() {
            None
        } else {
            Some(self.decision)
        }
    }

    pub(crate) const fn confirmation_label(&self) -> Option<&'static str> {
        match self.prompt {
            AccessPrompt::Entry { .. } => Some("Revoke"),
            AccessPrompt::All { .. } => Some("Revoke all"),
            AccessPrompt::Notice { .. } => None,
        }
    }

    pub(crate) const fn window_title(&self) -> &'static str {
        match self.prompt {
            AccessPrompt::Entry { .. } => "Splinterm — Confirm Revoking Agent Access",
            AccessPrompt::All { .. } => "Splinterm — Confirm Revoking All Access",
            AccessPrompt::Notice { .. } => "Splinterm — Agent Access",
        }
    }

    pub(crate) fn title_and_body(&self) -> (&'static str, String) {
        match &self.prompt {
            AccessPrompt::Entry { entry, changed, .. } => {
                let mut body = String::new();
                if *changed {
                    body.push_str("This access changed after you chose it; check it again.\n");
                }
                let _ = writeln!(
                    body,
                    "Revoke {} access for {} to {}?",
                    access_text(entry),
                    agent_text(entry),
                    workspace_text(entry)
                );
                let _ = writeln!(
                    body,
                    "Agent: {} ({})",
                    shown_name(&entry.identity.executable_path, MAX_PATH_SCALARS),
                    identity_kind_text(entry.identity.kind)
                );
                body.push_str(
                    "It stops at once and is removed from the remembered-grant store. Its subscriptions close and its control is released.",
                );
                if !entry.policy_rules.is_empty() {
                    let _ = write!(
                        body,
                        "\nAccess continues under policy rule {}; edit policy.json to remove it.",
                        rule_list(&entry.policy_rules)
                    );
                }
                ("REVOKE “ALWAYS” ACCESS", body)
            }
            AccessPrompt::All { counts, recounted } => {
                let mut body = String::new();
                if *recounted {
                    body.push_str(
                        "The numbers changed since you looked, and nothing was revoked. Check them again.\n",
                    );
                }
                body.push_str(&revoke_all_summary(*counts, false));
                body.push_str(
                    "\nEvery agent grant of every duration ends and waiting requests are cancelled. This Window keeps working.",
                );
                ("REVOKE ALL ACCESS", body)
            }
            AccessPrompt::Notice { title, body } => (*title, body.clone()),
        }
    }

    pub(crate) fn move_selection(&mut self) -> bool {
        if self.is_notice() {
            return false;
        }
        self.decision = match self.decision {
            TerminationDecision::Cancel => TerminationDecision::Terminate,
            TerminationDecision::Terminate => TerminationDecision::Cancel,
        };
        true
    }

    pub(crate) fn select(&mut self, decision: TerminationDecision) -> bool {
        if self.is_notice() || self.decision == decision {
            return false;
        }
        self.decision = decision;
        true
    }

    /// The access command the confirmed choice sends, or `None` on Cancel.
    pub(crate) fn command(&self) -> Option<WindowAccessCommand> {
        if self.decision != TerminationDecision::Terminate {
            return None;
        }
        match &self.prompt {
            AccessPrompt::Entry { target, .. } => Some(WindowAccessCommand::RevokeEntry {
                target: target.clone(),
            }),
            AccessPrompt::All { counts, .. } => {
                Some(WindowAccessCommand::RevokeAll { expected: *counts })
            }
            AccessPrompt::Notice { .. } => None,
        }
    }

    /// Fits an entry confirmation to a newer inventory. A changed entry is
    /// shown again with Cancel selected, so the person never confirms what
    /// they did not see; an entry that is gone has nothing left to revoke.
    pub(crate) fn reconcile(&mut self, snapshot: &AccessSnapshot) -> PromptReconcile {
        let AccessPrompt::Entry {
            target,
            entry,
            changed,
        } = &mut self.prompt
        else {
            return PromptReconcile::Unchanged;
        };
        let Some(current) = snapshot.entry(&target.key()) else {
            return PromptReconcile::Gone;
        };
        if revoke_target(current).as_ref() != Some(&*target) {
            return PromptReconcile::Gone;
        }
        if same_definition(current, entry) {
            return PromptReconcile::Unchanged;
        }
        **entry = current.clone();
        *changed = true;
        self.decision = TerminationDecision::Cancel;
        PromptReconcile::Changed
    }
}

/// Whether two reads of one entry show the person the same grant: use
/// counts and last-used time may change without asking again.
fn same_definition(left: &AccessEntry, right: &AccessEntry) -> bool {
    left.key == right.key
        && left.kind == right.kind
        && left.state == right.state
        && left.identity == right.identity
        && left.workspace == right.workspace
        && left.level == right.level
        && left.scopes == right.scopes
        && left.policy_rules == right.policy_rules
}

/// How far an agent reaches a workspace, Dojo, or Splint. Ordered, so the
/// strongest of several entries wins.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum AccessReach {
    #[default]
    None,
    /// A grant or policy rule covers it, but no agent it covers is connected.
    Reachable,
    /// An agent it covers is connected now.
    Connected,
}

impl AccessReach {
    const fn of(entry: &AccessEntry) -> Self {
        if entry.connections > 0 || entry.controllers > 0 {
            Self::Connected
        } else {
            Self::Reachable
        }
    }

    /// The mark in words, for the row and for assistive technology alike.
    pub(crate) const fn text(self, whole: bool) -> Option<&'static str> {
        match (self, whole) {
            (Self::None, _) => None,
            (Self::Reachable, true) => Some("Agent can reach"),
            (Self::Connected, true) => Some("Agent connected"),
            (Self::Reachable, false) => Some("Agent can reach part"),
            (Self::Connected, false) => Some("Agent connected to part"),
        }
    }
}

/// Policy scopes that administer authority rather than reach terminals; a
/// daemon-wide rule holding only these marks no workspace.
const fn administrative(scope: AutomationScope) -> bool {
    matches!(
        scope,
        AutomationScope::AuthorizationInspect
            | AutomationScope::AuthorizationRevoke
            | AutomationScope::AuditInspect
    )
}

/// What the Explorer marks, projected from one inventory read.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct AccessMarks {
    lairs: HashMap<LairId, AccessReach>,
    dojos: HashMap<DojoId, AccessReach>,
    splints: HashMap<SplintId, AccessReach>,
    waiting: HashMap<LairId, u32>,
    /// A daemon-wide policy rule reaches every workspace.
    every_lair: AccessReach,
}

impl AccessMarks {
    pub(crate) fn from_snapshot(snapshot: &AccessSnapshot) -> Self {
        fn raise<K: Eq + std::hash::Hash>(
            map: &mut HashMap<K, AccessReach>,
            key: K,
            reach: AccessReach,
        ) {
            let current = map.entry(key).or_default();
            *current = (*current).max(reach);
        }
        let mut marks = Self::default();
        for entry in &snapshot.entries {
            let reach = AccessReach::of(entry);
            match (entry.kind, entry.state) {
                (AccessEntryKind::Pending, _) => {
                    if let Some(workspace) = &entry.workspace {
                        let waiting = marks.waiting.entry(workspace.lair_id).or_default();
                        *waiting = waiting.saturating_add(1);
                    }
                }
                (AccessEntryKind::PersistentPolicy, AccessEntryState::Active) => {
                    if entry
                        .policy_scopes
                        .iter()
                        .all(|scope| administrative(*scope))
                    {
                        continue;
                    }
                    for resource in &entry.policy_resources {
                        match resource {
                            AccessPolicyResource::Daemon => {
                                marks.every_lair = marks.every_lair.max(reach);
                            }
                            AccessPolicyResource::Lair { lair_id } => {
                                raise(&mut marks.lairs, *lair_id, reach);
                            }
                            AccessPolicyResource::Dojo { dojo_id } => {
                                raise(&mut marks.dojos, *dojo_id, reach);
                            }
                            AccessPolicyResource::Splint { splint_id } => {
                                raise(&mut marks.splints, *splint_id, reach);
                            }
                        }
                    }
                }
                (
                    AccessEntryKind::Once
                    | AccessEntryKind::WorkspaceSession
                    | AccessEntryKind::Remembered,
                    AccessEntryState::Active,
                ) => {
                    if let Some(splint_id) = entry.splint_id {
                        raise(&mut marks.splints, splint_id, reach);
                    } else if let Some(workspace) = &entry.workspace {
                        raise(&mut marks.lairs, workspace.lair_id, reach);
                    }
                }
                // A missing workspace's entry, or one revoked but not yet
                // removed from the store, authorizes nothing.
                _ => {}
            }
        }
        marks
    }

    /// How far an agent reaches the whole workspace.
    pub(crate) fn lair(&self, lair_id: LairId) -> AccessReach {
        self.lairs
            .get(&lair_id)
            .copied()
            .unwrap_or_default()
            .max(self.every_lair)
    }

    pub(crate) fn dojo(&self, dojo_id: DojoId) -> AccessReach {
        self.dojos.get(&dojo_id).copied().unwrap_or_default()
    }

    pub(crate) fn splint(&self, splint_id: SplintId) -> AccessReach {
        self.splints.get(&splint_id).copied().unwrap_or_default()
    }

    /// Requests waiting for an answer about this workspace.
    pub(crate) fn waiting(&self, lair_id: LairId) -> u32 {
        self.waiting.get(&lair_id).copied().unwrap_or(0)
    }

    pub(crate) fn total_waiting(&self) -> u32 {
        self.waiting
            .values()
            .fold(0_u32, |total, count| total.saturating_add(*count))
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The waiting indicator in words, or `None` when nothing waits.
pub(crate) fn waiting_text(waiting: u32) -> Option<String> {
    match waiting {
        0 => None,
        1 => Some("Agent asking for access".to_owned()),
        count => Some(format!("{count} agents asking for access")),
    }
}

/// The Window title suffix while requests wait, so a person busy elsewhere
/// sees that a prompt is queued. The prompt itself never takes focus.
pub(crate) fn waiting_title(waiting: u32) -> Option<String> {
    match waiting {
        0 => None,
        1 => Some("AGENT ACCESS REQUEST WAITING".to_owned()),
        count => Some(format!("{count} AGENT ACCESS REQUESTS WAITING")),
    }
}

/// The marks one Explorer row shows, in words, joined for its status words.
pub(crate) fn mark_text(whole: AccessReach, part: AccessReach, waiting: u32) -> Option<String> {
    let mut parts = Vec::with_capacity(3);
    if let Some(text) = waiting_text(waiting) {
        parts.push(text);
    }
    if let Some(text) = whole.text(true) {
        parts.push(text.to_owned());
    }
    // Some of it is reached more strongly than all of it: say so too.
    if part > whole
        && let Some(text) = part.text(false)
    {
        parts.push(text.to_owned());
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use splinterm_protocol::{AccessIdentity, AccessLevel, AccessScope, AccessWorkspace};

    pub(crate) fn entry(
        key: AccessEntryKey,
        kind: AccessEntryKind,
        lair_id: LairId,
    ) -> AccessEntry {
        let policy = kind == AccessEntryKind::PersistentPolicy;
        let state = if kind == AccessEntryKind::Pending {
            AccessEntryState::Prompting
        } else {
            AccessEntryState::Active
        };
        AccessEntry {
            grant_id: match (&key, kind) {
                (AccessEntryKey::Grant { grant_id }, _) => Some(*grant_id),
                (_, AccessEntryKind::Remembered) => Some(91),
                _ => None,
            },
            key,
            kind,
            state,
            revocable: matches!(
                kind,
                AccessEntryKind::Once
                    | AccessEntryKind::WorkspaceSession
                    | AccessEntryKind::Remembered
            ),
            identity: AccessIdentity {
                kind: AccessIdentityKind::ProtectedInstall,
                executable_path: "/usr/bin/splinterm-mcp".into(),
                pid: None,
                display_name: Some("claude".into()),
            },
            workspace: (!policy).then(|| AccessWorkspace {
                lair_id,
                name: Some("work".into()),
            }),
            splint_id: None,
            incarnation: None,
            level: (!policy).then_some(AccessLevel::Manage),
            scopes: if policy {
                Vec::new()
            } else {
                AccessLevel::Manage.scopes().to_vec()
            },
            policy_scopes: if policy {
                vec![AutomationScope::TerminalVisibleRead]
            } else {
                Vec::new()
            },
            policy_resources: if policy {
                vec![AccessPolicyResource::Lair { lair_id }]
            } else {
                Vec::new()
            },
            created_at_unix_seconds: Some(1_760_000_000),
            last_used_at_unix_seconds: None,
            expires_at_unix_seconds: None,
            connections: 0,
            controllers: 0,
            policy_rules: Vec::new(),
        }
    }

    pub(crate) fn remembered_key() -> AccessEntryKey {
        AccessEntryKey::Remembered {
            entry_id: format!("ra_{}", "0123456789abcdef".repeat(2)),
        }
    }

    fn snapshot(revision: u64, entries: Vec<AccessEntry>) -> AccessSnapshot {
        let mut snapshot = AccessSnapshot::new(revision, RememberedStoreStatus::Available);
        snapshot.entries = entries;
        snapshot
    }

    fn inventory(lair_id: LairId) -> Vec<AccessEntry> {
        vec![
            entry(remembered_key(), AccessEntryKind::Remembered, lair_id),
            entry(
                AccessEntryKey::Grant { grant_id: 17 },
                AccessEntryKind::Once,
                lair_id,
            ),
            entry(
                AccessEntryKey::Grant { grant_id: 18 },
                AccessEntryKind::WorkspaceSession,
                lair_id,
            ),
            entry(
                AccessEntryKey::Pending { request_id: 3 },
                AccessEntryKind::Pending,
                lair_id,
            ),
            entry(
                AccessEntryKey::Policy {
                    rule_id: "ci".into(),
                },
                AccessEntryKind::PersistentPolicy,
                lair_id,
            ),
        ]
    }

    fn validated(entries: &[AccessEntry]) {
        for entry in entries {
            entry.validate().unwrap();
        }
    }

    #[test]
    fn the_list_shows_every_kind_with_a_word_tag_and_only_grants_are_revocable() {
        let lair_id = LairId::new();
        let entries = inventory(lair_id);
        validated(&entries);
        let ui = AgentAccessUi::new(Some(&snapshot(4, entries)), None);
        let rows = ui.rows();
        assert_eq!(
            rows.iter().map(|row| row.category).collect::<Vec<_>>(),
            ["ALWAYS", "ONCE", "OPEN", "WAITING", "POLICY"]
        );
        assert_eq!(
            rows.iter().map(|row| row.revocable).collect::<Vec<_>>(),
            [true, true, true, false, false]
        );
        assert_eq!(
            rows[0].title,
            format!(
                "‘claude’ · Manage · always for ‘work’ ({})",
                short_id(&lair_id)
            )
        );
        assert!(rows[2].title.contains("while ‘work’"));
        assert!(rows[3].title.starts_with("‘claude’ asks for Manage on"));
        assert!(rows[4].title.contains("policy rule ‘ci’"));
        assert_eq!(rows[3].status, "prompt showing");
        assert_eq!(rows[0].status, "not connected");
        assert!(rows[0].detail.starts_with("Enter revokes it (asks first)"));
        assert!(
            rows[0]
                .detail
                .contains("/usr/bin/splinterm-mcp (protected install)")
        );
        assert!(rows[0].detail.contains("never used"));
        assert!(rows[4].detail.contains("remove it from policy.json"));
        assert_eq!(ui.status_label(), "1 waiting");
    }

    #[test]
    fn policy_and_pending_rows_explain_instead_of_revoking() {
        let lair_id = LairId::new();
        let mut ui = AgentAccessUi::new(Some(&snapshot(4, inventory(lair_id))), None);
        assert!(ui.select_index(4));
        assert_eq!(ui.activate(), None);
        assert_eq!(
            ui.footer(),
            "Read-only here: remove it from policy.json to revoke"
        );
        assert!(ui.select_index(3));
        assert_eq!(ui.activate(), None);
        assert!(ui.footer().starts_with("Waiting for your answer"));
    }

    #[test]
    fn revoke_names_the_listed_id_kind_never_the_runtime_grant_of_a_remembered_entry() {
        let lair_id = LairId::new();
        let mut ui = AgentAccessUi::new(Some(&snapshot(4, inventory(lair_id))), None);
        // An "Always" grant asks first, and names its durable entry ID even
        // though its controllers carry runtime grant 91.
        let Some(AccessActivation::Confirm(mut prompt)) = ui.activate() else {
            panic!("an Always grant asks first");
        };
        assert_eq!(prompt.command(), None, "Cancel is preselected");
        assert!(prompt.move_selection());
        assert_eq!(
            prompt.command(),
            Some(WindowAccessCommand::RevokeEntry {
                target: AccessRevokeTarget::Remembered {
                    entry_id: format!("ra_{}", "0123456789abcdef".repeat(2)),
                },
            })
        );
        // Shorter grants revoke at once by their runtime grant ID.
        assert!(ui.select_index(1));
        assert_eq!(
            ui.activate(),
            Some(AccessActivation::Revoke(AccessRevokeTarget::Grant {
                grant_id: 17
            }))
        );
        // A second press while the daemon has not answered sends nothing.
        assert_eq!(ui.activate(), None);
        assert_eq!(ui.footer(), "Revoking…");
        assert!(ui.select_index(2));
        assert_eq!(
            ui.activate(),
            Some(AccessActivation::Revoke(AccessRevokeTarget::Grant {
                grant_id: 18
            }))
        );
        // A remembered entry is never revoked by a grant key, and a
        // mismatched key is never revocable.
        let mut mismatched = entry(remembered_key(), AccessEntryKind::Remembered, lair_id);
        mismatched.key = AccessEntryKey::Grant { grant_id: 91 };
        assert_eq!(revoke_target(&mismatched), None);
    }

    #[test]
    fn filtering_keeps_the_selected_entry_and_reports_no_match() {
        let lair_id = LairId::new();
        let mut ui = AgentAccessUi::new(Some(&snapshot(4, inventory(lair_id))), None);
        assert!(ui.select_index(2));
        assert!(ui.append_text("open"));
        assert_eq!(ui.rows().len(), 1);
        assert_eq!(ui.rows()[0].key, AccessEntryKey::Grant { grant_id: 18 });
        assert!(ui.clear_query());
        assert_eq!(ui.rows().len(), 5);
        assert_eq!(ui.selected_index(), 2, "the selection follows its entry");
        assert!(ui.append_text("POLICY ci"));
        assert_eq!(ui.rows().len(), 1);
        assert!(ui.clear_query());
        assert!(ui.append_text("ra_0123"));
        assert_eq!(ui.rows()[0].key, remembered_key());
        assert!(ui.clear_query());
        assert!(ui.append_text("nothing-like-this"));
        assert!(ui.rows().is_empty());
        assert_eq!(ui.empty_label().as_deref(), Some("No matching access"));
        assert_eq!(ui.activate(), None);
    }

    #[test]
    fn empty_loading_and_unavailable_states_are_explicit() {
        let ui = AgentAccessUi::new(None, None);
        assert_eq!(ui.empty_label().as_deref(), Some("Loading agent access…"));
        let ui = AgentAccessUi::new(Some(&snapshot(1, Vec::new())), None);
        assert_eq!(
            ui.empty_label().as_deref(),
            Some("No agent has access to any workspace.")
        );
        assert_eq!(ui.status_label(), "");
        let ui = AgentAccessUi::new(None, Some("not authorized"));
        assert_eq!(
            ui.empty_label().as_deref(),
            Some("Agent access unavailable: not authorized")
        );
        let mut store = snapshot(1, Vec::new());
        store.remembered_store = RememberedStoreStatus::Unavailable {
            reason: "corrupt\nstore".into(),
        };
        let ui = AgentAccessUi::new(Some(&store), None);
        assert!(ui.footer().contains("corrupt\\u{000a}store"));
    }

    #[test]
    fn names_are_escaped_and_bounded() {
        let lair_id = LairId::new();
        let mut hostile = entry(remembered_key(), AccessEntryKind::Remembered, lair_id);
        hostile.identity.display_name = Some("evil\u{202e}gnp.exe\n\u{1b}[2J".into());
        hostile.workspace.as_mut().unwrap().name = Some("w".repeat(400));
        hostile.identity.executable_path = "/opt/x\u{0007}y".into();
        let ui = AgentAccessUi::new(Some(&snapshot(1, vec![hostile])), None);
        let row = &ui.rows()[0];
        for text in [&row.title, &row.detail, &row.compact] {
            assert!(!text.chars().any(char::is_control), "{text}");
            assert!(!text.contains('\u{202e}'), "{text}");
        }
        assert!(
            row.title
                .contains("evil\\u{202e}gnp.exe\\u{000a}\\u{001b}[2J")
        );
        assert!(
            row.title
                .contains(&format!("{}…", "w".repeat(MAX_NAME_SCALARS)))
        );
        assert!(row.detail.contains("/opt/x\\u{0007}y"));
        assert_eq!(shown("abc", 2), "ab…");
    }

    #[test]
    fn a_newer_read_keeps_the_selection_and_drops_finished_revokes() {
        let lair_id = LairId::new();
        let mut ui = AgentAccessUi::new(Some(&snapshot(4, inventory(lair_id))), None);
        assert!(ui.select_index(1));
        assert!(matches!(ui.activate(), Some(AccessActivation::Revoke(_))));
        // The revoked grant is gone in the next read; the selection moves to
        // the entry that is now there, and nothing is "revoking" any more.
        let mut entries = inventory(lair_id);
        entries.remove(1);
        ui.apply_snapshot(&snapshot(5, entries.clone()));
        assert_eq!(ui.rows().len(), 4);
        assert_eq!(ui.rows()[ui.selected_index()].key, remembered_key());
        assert!(ui.select_index(1));
        assert!(matches!(ui.activate(), Some(AccessActivation::Revoke(_))));
        // A failed revoke may be tried again once the daemon answered it;
        // an answer about another entry leaves this one in flight.
        ui.finish_revoke(
            Some(&AccessEntryKey::Grant { grant_id: 99 }),
            "Revoke failed: not found",
        );
        assert_eq!(ui.activate(), None);
        ui.finish_revoke(
            Some(&AccessEntryKey::Grant { grant_id: 18 }),
            "Revoke failed: not found",
        );
        assert_eq!(ui.footer(), "Revoke failed: not found");
        assert!(matches!(ui.activate(), Some(AccessActivation::Revoke(_))));
    }

    #[test]
    fn a_stale_confirmation_is_shown_again_or_closed() {
        let lair_id = LairId::new();
        let entries = inventory(lair_id);
        let mut ui = AgentAccessUi::new(Some(&snapshot(4, entries.clone())), None);
        let Some(AccessActivation::Confirm(mut prompt)) = ui.activate() else {
            panic!("asks first");
        };
        assert!(prompt.move_selection());
        // Use counts and the revision may move without asking again.
        let mut used = entries.clone();
        used[0].connections = 2;
        used[0].last_used_at_unix_seconds = Some(1_760_000_100);
        assert_eq!(
            prompt.reconcile(&snapshot(5, used)),
            PromptReconcile::Unchanged
        );
        assert!(prompt.command().is_some());
        // A replaced definition must be checked again.
        let mut replaced = entries.clone();
        replaced[0].level = Some(AccessLevel::Look);
        replaced[0].scopes = AccessLevel::Look.scopes().to_vec();
        assert_eq!(
            prompt.reconcile(&snapshot(6, replaced)),
            PromptReconcile::Changed
        );
        assert_eq!(prompt.command(), None, "back on Cancel");
        let (_, body) = prompt.title_and_body();
        assert!(body.starts_with("This access changed after you chose it"));
        assert!(body.contains("Revoke Look access"));
        // Gone, or no longer revocable: nothing left to revoke.
        let mut gone = entries.clone();
        gone.remove(0);
        assert_eq!(prompt.reconcile(&snapshot(7, gone)), PromptReconcile::Gone);
        let mut pending = entries;
        pending[0].state = AccessEntryState::RemovalPending;
        pending[0].revocable = false;
        assert_eq!(
            prompt.reconcile(&snapshot(8, pending)),
            PromptReconcile::Gone
        );
    }

    #[test]
    fn revoke_all_confirms_exact_counts_and_recounts_on_mismatch() {
        let counts = AccessRevocationCounts {
            once: 1,
            workspace_session: 0,
            remembered: 2,
            pending_requests: 1,
        };
        let mut prompt = RevokeAccessUi::all(counts, false);
        let (title, body) = prompt.title_and_body();
        assert_eq!(title, "REVOKE ALL ACCESS");
        assert!(body.starts_with(
            "This removes 2 remembered grants, and ends 1 just-this-once grant, 0 while-open grants, and 1 pending request. Policy rules are not touched."
        ));
        assert_eq!(prompt.confirmation_label(), Some("Revoke all"));
        assert_eq!(prompt.command(), None, "Cancel is preselected");
        assert!(prompt.select(TerminationDecision::Terminate));
        assert_eq!(
            prompt.command(),
            Some(WindowAccessCommand::RevokeAll { expected: counts })
        );
        assert_eq!(check_revoke_all(counts, counts), RevokeAllCheck::Proceed);
        let grown = AccessRevocationCounts {
            remembered: 3,
            ..counts
        };
        assert_eq!(
            check_revoke_all(counts, grown),
            RevokeAllCheck::Recount(grown)
        );
        let recount = RevokeAccessUi::all(grown, true);
        let (_, body) = recount.title_and_body();
        assert!(body.starts_with("The numbers changed since you looked, and nothing was revoked."));
        assert!(body.contains("This removes 3 remembered grants"));
        assert!(counts_are_empty(AccessRevocationCounts::default()));
        assert!(!counts_are_empty(counts));
        assert_eq!(
            revoke_all_summary(counts, true),
            "Removed 2 remembered grants, and ended 1 just-this-once grant, 0 while-open grants, and 1 pending request. Policy rules are not touched."
        );
        let notice = RevokeAccessUi::notice("REVOKE ALL ACCESS", "Nothing to revoke.");
        assert!(notice.is_notice());
        assert_eq!(notice.decision(), None);
        assert_eq!(notice.confirmation_label(), None);
        assert_eq!(notice.command(), None);
    }

    #[test]
    fn snapshot_counts_waiting_requests_and_revoke_all_targets() {
        let lair_id = LairId::new();
        let all = snapshot(1, inventory(lair_id));
        assert_eq!(all.waiting(), 1);
        assert!(all.revoke_all_has_targets());
        let policy_only = snapshot(1, vec![inventory(lair_id).remove(4)]);
        assert_eq!(policy_only.waiting(), 0);
        assert!(!policy_only.revoke_all_has_targets());
        assert_eq!(waiting_title(0), None);
        assert_eq!(
            waiting_title(1).as_deref(),
            Some("AGENT ACCESS REQUEST WAITING")
        );
        assert_eq!(
            waiting_title(2).as_deref(),
            Some("2 AGENT ACCESS REQUESTS WAITING")
        );
    }

    #[test]
    fn marks_distinguish_reachable_from_connected_and_count_waiting() {
        let reachable = LairId::new();
        let connected = LairId::new();
        let asking = LairId::new();
        let splint_id = SplintId::new();
        let mut session = entry(
            AccessEntryKey::Grant { grant_id: 18 },
            AccessEntryKind::WorkspaceSession,
            reachable,
        );
        session.connections = 0;
        let mut remembered = entry(remembered_key(), AccessEntryKind::Remembered, connected);
        remembered.connections = 1;
        let mut splint = entry(
            AccessEntryKey::Grant { grant_id: 19 },
            AccessEntryKind::Once,
            asking,
        );
        splint.splint_id = Some(splint_id);
        splint.incarnation = Some(2);
        splint.controllers = 1;
        let pending = entry(
            AccessEntryKey::Pending { request_id: 3 },
            AccessEntryKind::Pending,
            asking,
        );
        let mut missing = entry(
            AccessEntryKey::Remembered {
                entry_id: format!("ra_{}", "f".repeat(32)),
            },
            AccessEntryKind::Remembered,
            LairId::new(),
        );
        missing.state = AccessEntryState::WorkspaceMissing;
        let entries = vec![remembered, missing.clone(), session, splint, pending];
        validated(&entries);
        let marks = AccessMarks::from_snapshot(&snapshot(1, entries));
        assert_eq!(marks.lair(reachable), AccessReach::Reachable);
        assert_eq!(marks.lair(connected), AccessReach::Connected);
        // A Splint grant marks the Splint, not its whole workspace.
        assert_eq!(marks.lair(asking), AccessReach::None);
        assert_eq!(marks.splint(splint_id), AccessReach::Connected);
        assert_eq!(marks.waiting(asking), 1);
        assert_eq!(marks.total_waiting(), 1);
        // A grant that authorizes nothing marks nothing.
        assert_eq!(
            marks.lair(missing.workspace.unwrap().lair_id),
            AccessReach::None
        );
        assert_eq!(
            mark_text(AccessReach::Connected, AccessReach::None, 1).as_deref(),
            Some("Agent asking for access · Agent connected")
        );
        assert_eq!(
            mark_text(AccessReach::None, AccessReach::Reachable, 0).as_deref(),
            Some("Agent can reach part")
        );
        assert_eq!(mark_text(AccessReach::None, AccessReach::None, 0), None);
        assert!(AccessMarks::default().is_empty());
    }

    #[test]
    fn policy_marks_follow_their_resources_and_ignore_administrative_rules() {
        let lair_id = LairId::new();
        let dojo_id = DojoId::new();
        let mut daemon = entry(
            AccessEntryKey::Policy {
                rule_id: "admin".into(),
            },
            AccessEntryKind::PersistentPolicy,
            lair_id,
        );
        daemon.policy_resources = vec![AccessPolicyResource::Daemon];
        daemon.policy_scopes = vec![AutomationScope::AuthorizationInspect];
        let marks = AccessMarks::from_snapshot(&snapshot(1, vec![daemon.clone()]));
        assert!(marks.is_empty(), "an inspect-only rule reaches no terminal");
        daemon
            .policy_scopes
            .push(AutomationScope::TerminalVisibleRead);
        daemon.connections = 1;
        let mut dojo = entry(
            AccessEntryKey::Policy {
                rule_id: "dojo".into(),
            },
            AccessEntryKind::PersistentPolicy,
            lair_id,
        );
        dojo.policy_resources = vec![AccessPolicyResource::Dojo { dojo_id }];
        let marks = AccessMarks::from_snapshot(&snapshot(1, vec![daemon, dojo]));
        assert_eq!(marks.lair(LairId::new()), AccessReach::Connected);
        assert_eq!(marks.dojo(dojo_id), AccessReach::Reachable);
    }

    #[test]
    fn custom_scopes_and_splint_grants_are_named_exactly() {
        let lair_id = LairId::new();
        let mut custom = entry(
            AccessEntryKey::Grant { grant_id: 20 },
            AccessEntryKind::Once,
            lair_id,
        );
        custom.level = None;
        custom.scopes = vec![AccessScope::Observe, AccessScope::ControlTakeover];
        custom.splint_id = Some(SplintId::new());
        custom.incarnation = Some(1);
        custom.identity.display_name = None;
        custom.identity.kind = AccessIdentityKind::Process;
        custom.identity.pid = Some(4242);
        custom.expires_at_unix_seconds = Some(0);
        validated(std::slice::from_ref(&custom));
        let ui = AgentAccessUi::new(Some(&snapshot(1, vec![custom])), None);
        let row = &ui.rows()[0];
        assert!(
            row.title.starts_with(
                "splinterm-mcp · Custom (observe, control_takeover) · once for Splint "
            )
        );
        assert!(row.detail.contains("(one process) · process 4242"));
        assert!(row.detail.contains("ends 1970-01-01 00:00 UTC"));
    }

    #[test]
    fn a_selection_whose_entry_left_the_list_does_nothing_until_chosen_again() {
        let lair_id = LairId::new();
        let mut ui = AgentAccessUi::new(Some(&snapshot(4, inventory(lair_id))), None);
        assert!(ui.select_index(1));
        // Grant 17 ends; grant 18 slides under the selection.
        let mut entries = inventory(lair_id);
        entries.remove(1);
        ui.apply_snapshot(&snapshot(5, entries.clone()));
        assert_eq!(ui.activate(), None, "nothing is revoked");
        assert_eq!(ui.footer(), "That access ended; choose again");
        // A newer read that still lists the (new) selection keeps it stale.
        ui.apply_snapshot(&snapshot(6, entries.clone()));
        assert_eq!(ui.activate(), None);
        // Any move is a new choice, even one that lands where it was.
        assert!(ui.move_selection(-1));
        assert!(ui.move_selection(1));
        assert_eq!(
            ui.activate(),
            Some(AccessActivation::Revoke(AccessRevokeTarget::Grant {
                grant_id: 18
            }))
        );
        // Typing a search is a new choice too, and so are Home and End.
        let mut ui = AgentAccessUi::new(Some(&snapshot(4, inventory(lair_id))), None);
        ui.mark_selection_stale("That access ended before you confirmed; nothing was revoked");
        assert_eq!(
            ui.footer(),
            "That access ended before you confirmed; nothing was revoked"
        );
        assert_eq!(ui.activate(), None);
        assert!(ui.select_first(), "clearing staleness is a change");
        assert!(matches!(ui.activate(), Some(AccessActivation::Confirm(_))));
        ui.mark_selection_stale("ended");
        assert!(ui.append_text("open"));
        assert!(matches!(ui.activate(), Some(AccessActivation::Revoke(_))));
    }

    #[test]
    fn a_click_selects_only_the_entry_it_pressed_and_never_activates() {
        let lair_id = LairId::new();
        let mut ui = AgentAccessUi::new(Some(&snapshot(4, inventory(lair_id))), None);
        let pressed = ui.row_key(1).cloned().unwrap();
        // Out of range never falls through to a selection.
        assert!(!ui.release_click(&pressed, 99));
        assert_eq!(ui.row_key(99), None);
        // Released on another entry: nothing.
        assert!(!ui.release_click(&pressed, 2));
        assert_eq!(ui.selected_index(), 0);
        // A click selects; another on the selected row only selects again.
        assert!(ui.release_click(&pressed, 1));
        assert_eq!(ui.selected_index(), 1);
        assert!(ui.release_click(&pressed, 1));
        assert_eq!(ui.selected_index(), 1);
        assert!(!ui.footer().contains("Revoking"), "{}", ui.footer());
        // A refresh between press and release moves another entry under
        // the pointer: the release is ignored.
        let mut entries = inventory(lair_id);
        entries.remove(1);
        ui.apply_snapshot(&snapshot(5, entries));
        assert!(!ui.release_click(&pressed, 1));
        // On a stale selection Enter waits for a new choice, and a click on
        // the selected row is that choice, still without acting.
        assert_eq!(ui.activate(), None);
        assert_eq!(ui.footer(), "That access ended; choose again");
        let now_there = ui.row_key(0).cloned().unwrap();
        assert_eq!(ui.selected_index(), 0);
        assert!(ui.release_click(&now_there, 0));
        assert!(ui.release_click(&now_there, 0));
        // Enter is what acts: the "Always" grant asks first.
        assert!(matches!(ui.activate(), Some(AccessActivation::Confirm(_))));
    }

    #[test]
    fn a_click_never_revokes_the_row_a_revoke_moved_the_selection_to() {
        let lair_id = LairId::new();
        let mut ui = AgentAccessUi::new(Some(&snapshot(4, inventory(lair_id))), None);
        // Enter revokes the just-this-once grant at once.
        assert!(ui.select_index(1));
        assert_eq!(
            ui.activate(),
            Some(AccessActivation::Revoke(AccessRevokeTarget::Grant {
                grant_id: 17
            }))
        );
        // The next read no longer lists it, and the selection lands on
        // another row.
        let mut entries = inventory(lair_id);
        entries.remove(1);
        ui.apply_snapshot(&snapshot(5, entries));
        ui.finish_revoke(Some(&AccessEntryKey::Grant { grant_id: 17 }), "Revoked");
        let target = ui.row_key(1).cloned().unwrap();
        assert_eq!(target, AccessEntryKey::Grant { grant_id: 18 });
        // Clicking that row to choose it, once or again, only selects it.
        assert!(ui.release_click(&target, 1));
        assert_eq!(ui.selected_index(), 1);
        assert!(ui.release_click(&target, 1));
        assert!(ui.release_click(&target, 1));
        assert!(!ui.footer().contains("Revoking"), "{}", ui.footer());
        // Nothing was revoked or sent: Enter still revokes it, once.
        assert_eq!(
            ui.activate(),
            Some(AccessActivation::Revoke(AccessRevokeTarget::Grant {
                grant_id: 18
            }))
        );
        assert_eq!(ui.activate(), None);
        assert_eq!(ui.footer(), "Revoking…");
    }

    #[test]
    fn invisible_separators_and_row_punctuation_in_names_are_escaped() {
        for character in [
            '\u{200b}', '\u{200c}', '\u{200d}', '\u{2028}', '\u{2029}', '\u{2060}', '\u{2063}',
            '\u{2065}', '\u{feff}', '\u{061c}', '\u{202e}', '\u{2066}', '\u{0085}',
        ] {
            assert!(unsafe_display_character(character), "{character:?}");
            assert_eq!(
                shown(&format!("a{character}b"), 64),
                format!("a\\u{{{:04x}}}b", u32::from(character))
            );
        }
        for character in ['a', '‘', '·', 'é', '中'] {
            assert!(!unsafe_display_character(character), "{character:?}");
        }
        // Inside a row a name cannot fake the quotes or separators around it.
        let lair_id = LairId::new();
        let mut fake = entry(remembered_key(), AccessEntryKind::Remembered, lair_id);
        fake.identity.display_name = Some("x’ · y".into());
        fake.workspace.as_mut().unwrap().name = Some("w\u{2028}v".into());
        fake.identity.executable_path = "/opt/a · b".into();
        let ui = AgentAccessUi::new(Some(&snapshot(1, vec![fake])), None);
        let row = &ui.rows()[0];
        assert!(
            row.title
                .starts_with("‘x\\u{2019} \\u{00b7} y’ · Manage · always for ‘w\\u{2028}v’"),
            "{}",
            row.title
        );
        assert!(
            row.detail
                .contains("/opt/a \\u{00b7} b (protected install)")
        );
        // Notices keep their own punctuation.
        assert_eq!(shown("a · b", 16), "a · b");
    }

    #[test]
    fn characters_that_draw_nothing_are_escaped_but_emoji_selectors_stay() {
        for hidden in [
            '\u{00ad}',
            '\u{034f}',
            '\u{115f}',
            '\u{17b4}',
            '\u{180e}',
            '\u{3164}',
            '\u{ffa0}',
            '\u{fff9}',
            '\u{1bca0}',
            '\u{1d173}',
            '\u{e0001}',
            '\u{e0041}',
        ] {
            assert!(unsafe_display_character(hidden), "{hidden:?}");
        }
        assert_eq!(
            shown("/usr/bin/claude\u{e0041}\u{e0042}", 64),
            "/usr/bin/claude\\u{e0041}\\u{e0042}"
        );
        assert!(!unsafe_display_character('\u{fe0f}'));
        assert_eq!(
            shown("deploy \u{2764}\u{fe0f}", 64),
            "deploy \u{2764}\u{fe0f}"
        );
    }

    /// The consent prompt escapes as `shown_name` does (plus its own `'`), so
    /// a hostile name reads the same in the prompt and in the list: no whitespace is
    /// collapsed, and tabs and newlines are escaped like any control.
    #[test]
    fn a_hostile_agent_name_is_listed_in_the_prompts_escaped_form() {
        let hostile = "evil\u{1b}[31mRED\u{202e}gpj.exe\u{7}\tend";
        let escaped = "evil\\u{001b}[31mRED\\u{202e}gpj.exe\\u{0007}\\u{0009}end";
        assert_eq!(shown_name(hostile, 64), escaped);
        assert_eq!(shown_name("a  b\nc", 64), "a  b\\u{000a}c");
        let lair_id = LairId::new();
        let mut agent = entry(remembered_key(), AccessEntryKind::Remembered, lair_id);
        agent.identity.display_name = Some("gpj\u{202e}\u{7}\tx".into());
        let ui = AgentAccessUi::new(Some(&snapshot(1, vec![agent])), None);
        let row = &ui.rows()[0];
        assert!(
            row.title
                .starts_with("‘gpj\\u{202e}\\u{0007}\\u{0009}x’ · "),
            "{}",
            row.title
        );
        assert!(!row.title.chars().any(unsafe_display_character));
    }

    #[test]
    fn a_stronger_partial_mark_is_shown_beside_the_whole_one() {
        assert_eq!(
            mark_text(AccessReach::Reachable, AccessReach::Connected, 0).as_deref(),
            Some("Agent can reach · Agent connected to part")
        );
        assert_eq!(
            mark_text(AccessReach::Connected, AccessReach::Reachable, 0).as_deref(),
            Some("Agent connected")
        );
        assert_eq!(
            mark_text(AccessReach::Reachable, AccessReach::Reachable, 1).as_deref(),
            Some("Agent asking for access · Agent can reach")
        );
    }

    #[test]
    fn a_truncated_read_says_so_in_the_always_visible_status_label() {
        let lair_id = LairId::new();
        let mut read = snapshot(1, inventory(lair_id));
        read.truncated = true;
        let ui = AgentAccessUi::new(Some(&read), None);
        assert_eq!(ui.status_label(), "1 waiting · first 512 only");
        read.entries.remove(3);
        let ui = AgentAccessUi::new(Some(&read), None);
        assert_eq!(ui.status_label(), "first 512 only");
        assert_eq!(AgentAccessUi::new(None, None).status_label(), "");
    }
}
