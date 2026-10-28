use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{Axis, LayoutNode, Splint, SplintId, SplintState, SplitRatio, SplitSide};

const MAX_NAME_BYTES: usize = 128;

/// The collision-resistant name a graphical launch gives a Lair the user did
/// not name, from a timestamp in nanoseconds and the launching process ID.
#[must_use]
pub fn generated_lair_name(stamp_nanos: u128, process_id: u32) -> String {
    format!("terminal-{stamp_nanos}-{process_id}")
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TopologyRevision(pub(crate) u64);

impl TopologyRevision {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LairId(Uuid);

impl LairId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    #[must_use]
    pub(crate) const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }
}

impl Default for LairId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for LairId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::str::FromStr for LairId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value).map(Self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DojoId(Uuid);

impl DojoId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    #[must_use]
    pub(crate) const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }
}

impl Default for DojoId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for DojoId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::str::FromStr for DojoId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value).map(Self)
    }
}

/// One persistent terminal layout within a Lair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dojo {
    pub id: DojoId,
    pub name: String,
    /// Daemon-persisted convenience hint; connected clients keep actual focus locally.
    pub default_focus: SplintId,
    pub root: LayoutNode,
}

impl Dojo {
    #[must_use]
    pub fn with_shell(name: impl Into<String>, cwd: PathBuf) -> Self {
        let splint = Splint::shell(cwd);
        Self {
            id: DojoId::new(),
            name: name.into(),
            default_focus: splint.id,
            root: LayoutNode::Leaf(splint),
        }
    }
}

/// In-memory lifetime policy for a Lair.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LairLifetime {
    #[default]
    Persistent,
    Transient,
}

impl LairLifetime {
    #[must_use]
    pub const fn is_persistent(&self) -> bool {
        matches!(self, Self::Persistent)
    }
}

/// Durable retention policy for a persistent Lair recipe.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LairRetention {
    /// Eligible for bounded retirement only after every Splint has exited.
    #[default]
    Disposable,
    /// Explicitly saved and protected from automatic retirement.
    Saved,
    /// Explicitly pinned and protected from automatic retirement.
    Pinned,
}

impl LairRetention {
    #[must_use]
    pub const fn is_protected(self) -> bool {
        matches!(self, Self::Saved | Self::Pinned)
    }
}

/// A named session containing Dojos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lair {
    pub id: LairId,
    pub name: String,
    #[serde(default, skip_serializing_if = "LairLifetime::is_persistent")]
    pub lifetime: LairLifetime,
    #[serde(default)]
    pub retention: LairRetention,
    /// Bounded, presentation-only origin label retained independently of its source file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    pub dojos: Vec<Dojo>,
}

impl Lair {
    #[must_use]
    pub fn new(name: impl Into<String>, cwd: PathBuf) -> Self {
        Self::with_lifetime(name, cwd, LairLifetime::Persistent)
    }

    #[must_use]
    pub fn transient(name: impl Into<String>, cwd: PathBuf) -> Self {
        Self::with_lifetime(name, cwd, LairLifetime::Transient)
    }

    fn with_lifetime(name: impl Into<String>, cwd: PathBuf, lifetime: LairLifetime) -> Self {
        Self {
            id: LairId::new(),
            name: name.into(),
            lifetime,
            retention: LairRetention::Disposable,
            provenance: None,
            dojos: vec![Dojo::with_shell("Dojo 1", cwd)],
        }
    }

    /// Returns the next implicit Dojo name without reusing numeric gaps.
    ///
    /// # Errors
    ///
    /// Returns [`TopologyError::DefaultDojoNameExhausted`] when the highest
    /// canonical numbered name is `Dojo 18446744073709551615`.
    pub fn next_default_dojo_name(&self) -> Result<String, TopologyError> {
        let highest = self
            .dojos
            .iter()
            .filter_map(|dojo| {
                let suffix = dojo.name.strip_prefix("Dojo ")?;
                let number = suffix.parse::<u64>().ok()?;
                (number > 0 && suffix == number.to_string()).then_some(number)
            })
            .max()
            .unwrap_or(0);
        let next = highest
            .checked_add(1)
            .ok_or(TopologyError::DefaultDojoNameExhausted)?;
        Ok(format!("Dojo {next}"))
    }
}

/// The daemon-owned collection of live Lairs.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Topology {
    pub(crate) revision: TopologyRevision,
    pub(crate) lairs: BTreeMap<LairId, Lair>,
}

#[allow(
    clippy::missing_errors_doc,
    reason = "transaction errors are exhaustively represented by TopologyError"
)]
impl Topology {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            revision: TopologyRevision(0),
            lairs: BTreeMap::new(),
        }
    }

    /// Adds a named Lair with one initial shell Dojo.
    pub fn create_lair(
        &mut self,
        name: impl Into<String>,
        cwd: PathBuf,
    ) -> Result<&Lair, TopologyError> {
        self.create_lair_at(self.revision, name, cwd)
    }

    pub fn create_lair_at(
        &mut self,
        expected: TopologyRevision,
        name: impl Into<String>,
        cwd: PathBuf,
    ) -> Result<&Lair, TopologyError> {
        self.check_revision(expected)?;
        let name = normalized_name(&name.into())?;
        if self.lairs.values().any(|lair| lair.name == name) {
            return Err(TopologyError::DuplicateLairName(name));
        }

        let lair = Lair::new(name, cwd);
        let id = lair.id;
        self.insert_lair_at(expected, lair)?;
        Ok(&self.lairs[&id])
    }

    pub fn insert_lair_at(
        &mut self,
        expected: TopologyRevision,
        lair: Lair,
    ) -> Result<TopologyRevision, TopologyError> {
        self.check_revision(expected)?;
        validate_name(&lair.name)?;
        if self.lairs.contains_key(&lair.id) {
            return Err(TopologyError::DuplicateLairId(lair.id));
        }
        if self.lairs.values().any(|current| current.name == lair.name) {
            return Err(TopologyError::DuplicateLairName(lair.name));
        }
        self.validate_new_dojos(&lair.dojos)?;
        self.lairs.insert(lair.id, lair);
        self.advance_revision();
        Ok(self.revision)
    }

    pub fn remove_lair(&mut self, id: LairId) -> Option<Lair> {
        let removed = self.lairs.remove(&id);
        if removed.is_some() {
            self.advance_revision();
        }
        removed
    }

    /// Atomically replaces one Lair with another at one topology revision.
    pub fn replace_lair_at(
        &mut self,
        expected: TopologyRevision,
        removed_id: LairId,
        inserted: Lair,
    ) -> Result<TopologyRevision, TopologyError> {
        self.check_revision(expected)?;
        validate_name(&inserted.name)?;
        if removed_id == inserted.id || !self.lairs.contains_key(&removed_id) {
            return Err(TopologyError::LairNotFound(removed_id));
        }
        if self.lairs.contains_key(&inserted.id) {
            return Err(TopologyError::DuplicateLairId(inserted.id));
        }
        if self
            .lairs
            .values()
            .any(|current| current.id != removed_id && current.name == inserted.name)
        {
            return Err(TopologyError::DuplicateLairName(inserted.name));
        }
        self.validate_new_dojos(&inserted.dojos)?;
        self.lairs.remove(&removed_id);
        self.lairs.insert(inserted.id, inserted);
        self.advance_revision();
        Ok(self.revision)
    }

    /// Inserts a new leaf beside `target` and commits one topology revision.
    pub fn split_splint(
        &mut self,
        target: SplintId,
        new_splint: Splint,
        axis: Axis,
        side: SplitSide,
        ratio: SplitRatio,
    ) -> Result<TopologyRevision, TopologyError> {
        self.split_splint_at(self.revision, target, new_splint, axis, side, ratio)
    }

    pub fn split_splint_at(
        &mut self,
        expected: TopologyRevision,
        target: SplintId,
        new_splint: Splint,
        axis: Axis,
        side: SplitSide,
        ratio: SplitRatio,
    ) -> Result<TopologyRevision, TopologyError> {
        self.check_revision(expected)?;
        if self.find_splint(new_splint.id).is_some() {
            return Err(TopologyError::DuplicateSplintId(new_splint.id));
        }

        let dojo = self
            .lairs
            .values_mut()
            .flat_map(|lair| &mut lair.dojos)
            .find(|dojo| dojo.root.find_splint(target).is_some())
            .ok_or(TopologyError::SplintNotFound(target))?;
        if !dojo.root.split(target, new_splint, axis, side, ratio) {
            return Err(TopologyError::SplintNotFound(target));
        }

        self.advance_revision();
        Ok(self.revision)
    }

    /// Removes an exited leaf, collapsing its parent or removing its final Dojo.
    pub fn close_splint(&mut self, id: SplintId) -> Result<TopologyRevision, TopologyError> {
        self.close_splint_at(self.revision, id)
    }

    pub fn close_splint_at(
        &mut self,
        expected: TopologyRevision,
        id: SplintId,
    ) -> Result<TopologyRevision, TopologyError> {
        self.check_revision(expected)?;
        let splint = self
            .find_splint(id)
            .ok_or(TopologyError::SplintNotFound(id))?;
        if !matches!(splint.state, SplintState::Exited(_)) {
            return Err(TopologyError::SplintStillLive(id));
        }

        'lairs: for lair in self.lairs.values_mut() {
            for dojo_index in 0..lair.dojos.len() {
                if lair.dojos[dojo_index].root.find_splint(id).is_none() {
                    continue;
                }
                let root = lair.dojos[dojo_index].root.clone();
                match root.remove(id) {
                    Ok(Some(root)) => {
                        let dojo = &mut lair.dojos[dojo_index];
                        dojo.root = root;
                        if dojo.default_focus == id {
                            dojo.default_focus = dojo.root.first_splint_id();
                        }
                    }
                    Ok(None) => {
                        lair.dojos.remove(dojo_index);
                    }
                    Err(_) => return Err(TopologyError::SplintNotFound(id)),
                }
                break 'lairs;
            }
        }

        self.advance_revision();
        Ok(self.revision)
    }

    pub fn set_split_ratio_at(
        &mut self,
        expected: TopologyRevision,
        target: SplintId,
        ancestor: u16,
        ratio: SplitRatio,
    ) -> Result<TopologyRevision, TopologyError> {
        self.check_revision(expected)?;
        let dojo = self
            .lairs
            .values_mut()
            .flat_map(|lair| &mut lair.dojos)
            .find(|dojo| dojo.root.find_splint(target).is_some())
            .ok_or(TopologyError::SplintNotFound(target))?;
        if !dojo.root.set_ancestor_ratio(target, ancestor, ratio) {
            return Err(TopologyError::SplintHasNoParent(target));
        }
        self.advance_revision();
        Ok(self.revision)
    }

    pub fn new_dojo_at(
        &mut self,
        expected: TopologyRevision,
        lair_id: LairId,
        dojo: Dojo,
    ) -> Result<TopologyRevision, TopologyError> {
        self.new_dojo_with_promotion_at(expected, lair_id, dojo, false)
            .map(|(revision, _)| revision)
    }

    /// Atomically promotes a transient Lair when requested and appends one Dojo.
    pub fn new_dojo_with_promotion_at(
        &mut self,
        expected: TopologyRevision,
        lair_id: LairId,
        dojo: Dojo,
        promote_transient_lair: bool,
    ) -> Result<(TopologyRevision, bool), TopologyError> {
        self.check_revision(expected)?;
        self.validate_new_dojos(std::slice::from_ref(&dojo))?;
        let lair = self
            .lairs
            .get_mut(&lair_id)
            .ok_or(TopologyError::LairNotFound(lair_id))?;
        let promoted = promote_transient_lair && lair.lifetime == LairLifetime::Transient;
        if promoted {
            lair.lifetime = LairLifetime::Persistent;
        }
        lair.dojos.push(dojo);
        self.advance_revision();
        Ok((self.revision, promoted))
    }

    /// Atomically appends a complete bounded Dojo set and optionally renames its Lair.
    pub fn materialize_dojos_at(
        &mut self,
        expected: TopologyRevision,
        lair_id: LairId,
        rename: Option<String>,
        dojos: Vec<Dojo>,
    ) -> Result<TopologyRevision, TopologyError> {
        self.check_revision(expected)?;
        if dojos.is_empty() {
            return Err(TopologyError::EmptyDojoSet);
        }
        self.validate_new_dojos(&dojos)?;
        let rename = rename.map(|name| normalized_name(&name)).transpose()?;
        if let Some(name) = rename.as_ref()
            && self
                .lairs
                .values()
                .any(|lair| lair.id != lair_id && lair.name == *name)
        {
            return Err(TopologyError::DuplicateLairName(name.clone()));
        }
        let lair = self
            .lairs
            .get_mut(&lair_id)
            .ok_or(TopologyError::LairNotFound(lair_id))?;
        if let Some(rename) = rename {
            lair.name = rename;
        }
        lair.dojos.extend(dojos);
        self.advance_revision();
        Ok(self.revision)
    }

    /// Atomically inserts a new Lair containing only the supplied complete Dojos.
    pub fn materialize_lair_at(
        &mut self,
        expected: TopologyRevision,
        name: impl Into<String>,
        dojos: Vec<Dojo>,
    ) -> Result<(LairId, TopologyRevision), TopologyError> {
        self.materialize_lair_with_provenance_at(expected, name, dojos, None)
    }

    pub fn materialize_lair_with_provenance_at(
        &mut self,
        expected: TopologyRevision,
        name: impl Into<String>,
        dojos: Vec<Dojo>,
        provenance: Option<String>,
    ) -> Result<(LairId, TopologyRevision), TopologyError> {
        self.check_revision(expected)?;
        if dojos.is_empty() {
            return Err(TopologyError::EmptyDojoSet);
        }
        let name = normalized_name(&name.into())?;
        if self.lairs.values().any(|lair| lair.name == name) {
            return Err(TopologyError::DuplicateLairName(name));
        }
        self.validate_new_dojos(&dojos)?;
        let lair_id = LairId::new();
        self.lairs.insert(
            lair_id,
            Lair {
                id: lair_id,
                name,
                lifetime: LairLifetime::Persistent,
                retention: LairRetention::Disposable,
                provenance,
                dojos,
            },
        );
        self.advance_revision();
        Ok((lair_id, self.revision))
    }

    pub fn close_dojo_at(
        &mut self,
        expected: TopologyRevision,
        dojo_id: DojoId,
    ) -> Result<TopologyRevision, TopologyError> {
        self.remove_dojo_at(expected, dojo_id, false)
    }

    /// Removes a Dojo whose live processes the caller is terminating, as
    /// [`Self::terminate_lair_at`] does for a whole Lair.
    pub fn terminate_dojo_at(
        &mut self,
        expected: TopologyRevision,
        dojo_id: DojoId,
    ) -> Result<TopologyRevision, TopologyError> {
        self.remove_dojo_at(expected, dojo_id, true)
    }

    fn remove_dojo_at(
        &mut self,
        expected: TopologyRevision,
        dojo_id: DojoId,
        allow_live: bool,
    ) -> Result<TopologyRevision, TopologyError> {
        self.check_revision(expected)?;
        let lair = self
            .lairs
            .values_mut()
            .find(|lair| lair.dojos.iter().any(|dojo| dojo.id == dojo_id))
            .ok_or(TopologyError::DojoNotFound(dojo_id))?;
        let index = lair
            .dojos
            .iter()
            .position(|dojo| dojo.id == dojo_id)
            .ok_or(TopologyError::DojoNotFound(dojo_id))?;
        if !allow_live && !lair.dojos[index].root.all_exited() {
            return Err(TopologyError::DojoStillLive(dojo_id));
        }
        lair.dojos.remove(index);
        self.advance_revision();
        Ok(self.revision)
    }

    pub fn terminate_lair_at(
        &mut self,
        expected: TopologyRevision,
        lair_id: LairId,
    ) -> Result<TopologyRevision, TopologyError> {
        self.check_revision(expected)?;
        self.lairs
            .remove(&lair_id)
            .ok_or(TopologyError::LairNotFound(lair_id))?;
        self.advance_revision();
        Ok(self.revision)
    }

    pub fn set_lair_retention_at(
        &mut self,
        expected: TopologyRevision,
        lair_id: LairId,
        retention: LairRetention,
    ) -> Result<TopologyRevision, TopologyError> {
        self.check_revision(expected)?;
        let lair = self
            .lairs
            .get_mut(&lair_id)
            .ok_or(TopologyError::LairNotFound(lair_id))?;
        if !lair.lifetime.is_persistent() {
            return Err(TopologyError::TransientLairRetention(lair_id));
        }
        lair.retention = retention;
        self.advance_revision();
        Ok(self.revision)
    }

    pub fn rename_lair_at(
        &mut self,
        expected: TopologyRevision,
        lair_id: LairId,
        name: impl Into<String>,
    ) -> Result<TopologyRevision, TopologyError> {
        self.check_revision(expected)?;
        let name = normalized_name(&name.into())?;
        if self
            .lairs
            .values()
            .any(|lair| lair.id != lair_id && lair.name == name)
        {
            return Err(TopologyError::DuplicateLairName(name));
        }
        self.lairs
            .get_mut(&lair_id)
            .ok_or(TopologyError::LairNotFound(lair_id))?
            .name = name;
        self.advance_revision();
        Ok(self.revision)
    }

    pub fn rename_dojo_at(
        &mut self,
        expected: TopologyRevision,
        dojo_id: DojoId,
        name: impl Into<String>,
    ) -> Result<TopologyRevision, TopologyError> {
        self.rename_dojo_with_promotion_at(expected, dojo_id, name, false)
            .map(|(revision, _)| revision)
    }

    /// Atomically promotes a transient Lair when requested and renames one Dojo.
    pub fn rename_dojo_with_promotion_at(
        &mut self,
        expected: TopologyRevision,
        dojo_id: DojoId,
        name: impl Into<String>,
        promote_transient_lair: bool,
    ) -> Result<(TopologyRevision, bool), TopologyError> {
        self.check_revision(expected)?;
        let name = normalized_name(&name.into())?;
        let lair = self
            .lairs
            .values_mut()
            .find(|lair| lair.dojos.iter().any(|dojo| dojo.id == dojo_id))
            .ok_or(TopologyError::DojoNotFound(dojo_id))?;
        let promoted = promote_transient_lair && lair.lifetime == LairLifetime::Transient;
        if promoted {
            lair.lifetime = LairLifetime::Persistent;
        }
        lair.dojos
            .iter_mut()
            .find(|dojo| dojo.id == dojo_id)
            .ok_or(TopologyError::DojoNotFound(dojo_id))?
            .name = name;
        self.advance_revision();
        Ok((self.revision, promoted))
    }

    pub fn set_dojo_default_focus_at(
        &mut self,
        expected: TopologyRevision,
        dojo_id: DojoId,
        splint_id: SplintId,
    ) -> Result<TopologyRevision, TopologyError> {
        self.check_revision(expected)?;
        let dojo = self
            .find_dojo_mut(dojo_id)
            .ok_or(TopologyError::DojoNotFound(dojo_id))?;
        if dojo.root.find_splint(splint_id).is_none() {
            return Err(TopologyError::InvalidDojoDefaultFocus { dojo_id, splint_id });
        }
        dojo.default_focus = splint_id;
        self.advance_revision();
        Ok(self.revision)
    }

    pub fn rename_splint_at(
        &mut self,
        expected: TopologyRevision,
        splint_id: SplintId,
        title: impl Into<String>,
    ) -> Result<TopologyRevision, TopologyError> {
        self.check_revision(expected)?;
        let title = normalized_name(&title.into())?;
        let splint = self
            .find_splint_mut(splint_id)
            .ok_or(TopologyError::SplintNotFound(splint_id))?;
        splint.title = title;
        // Any rename pins the title, even to the default text.
        splint.title_set = true;
        self.advance_revision();
        Ok(self.revision)
    }

    /// Restores a Splint's default title and clears its rename, so windows
    /// may show its foreground app's name again.
    pub fn reset_splint_title_at(
        &mut self,
        expected: TopologyRevision,
        splint_id: SplintId,
    ) -> Result<TopologyRevision, TopologyError> {
        self.check_revision(expected)?;
        let splint = self
            .find_splint_mut(splint_id)
            .ok_or(TopologyError::SplintNotFound(splint_id))?;
        crate::DEFAULT_SPLINT_TITLE.clone_into(&mut splint.title);
        splint.title_set = false;
        self.advance_revision();
        Ok(self.revision)
    }

    /// Replaces launch metadata and marks an exited Splint running without changing topology.
    pub fn commit_relaunch(
        &mut self,
        id: SplintId,
        cwd: PathBuf,
        command: Vec<String>,
    ) -> Result<(), TopologyError> {
        let splint = self
            .find_splint_mut(id)
            .ok_or(TopologyError::SplintNotFound(id))?;
        if !matches!(splint.state, SplintState::Exited(_)) {
            return Err(TopologyError::SplintStillLive(id));
        }
        splint.cwd = cwd;
        splint.command = command;
        splint.state = SplintState::Running;
        Ok(())
    }

    pub fn set_splint_launch_metadata(
        &mut self,
        id: SplintId,
        launch: crate::SplintLaunchMetadata,
    ) -> bool {
        self.find_splint_mut(id).is_some_and(|splint| {
            *splint.launch = launch;
            true
        })
    }

    pub fn set_splint_last_incarnation(&mut self, id: SplintId, incarnation: u64) -> bool {
        self.find_splint_mut(id).is_some_and(|splint| {
            splint.last_incarnation = Some(incarnation);
            true
        })
    }

    pub fn set_splint_dimensions(&mut self, id: SplintId, columns: u16, rows: u16) -> bool {
        self.find_splint_mut(id).is_some_and(|splint| {
            let changed = splint.launch.columns != columns || splint.launch.rows != rows;
            splint.launch.columns = columns;
            splint.launch.rows = rows;
            changed
        })
    }

    pub fn set_splint_state(&mut self, id: SplintId, state: SplintState) -> bool {
        self.find_splint_mut(id).is_some_and(|splint| {
            splint.state = state;
            true
        })
    }

    #[must_use]
    pub fn revision(&self) -> TopologyRevision {
        self.revision
    }

    #[must_use]
    pub fn find_lair(&self, id: LairId) -> Option<&Lair> {
        self.lairs.get(&id)
    }

    #[must_use]
    pub fn find_dojo(&self, id: DojoId) -> Option<&Dojo> {
        self.lairs
            .values()
            .flat_map(|lair| &lair.dojos)
            .find(|dojo| dojo.id == id)
    }

    #[must_use]
    pub fn find_splint(&self, id: SplintId) -> Option<&Splint> {
        self.lairs
            .values()
            .flat_map(|lair| &lair.dojos)
            .find_map(|dojo| dojo.root.find_splint(id))
    }

    fn find_dojo_mut(&mut self, id: DojoId) -> Option<&mut Dojo> {
        self.lairs
            .values_mut()
            .flat_map(|lair| &mut lair.dojos)
            .find(|dojo| dojo.id == id)
    }

    fn find_splint_mut(&mut self, id: SplintId) -> Option<&mut Splint> {
        self.lairs
            .values_mut()
            .flat_map(|lair| &mut lair.dojos)
            .find_map(|dojo| dojo.root.find_splint_mut(id))
    }

    #[must_use]
    pub fn lairs(&self) -> impl ExactSizeIterator<Item = &Lair> {
        self.lairs.values()
    }

    /// A read-only view holding only the Lairs `keep` selects, at the same
    /// revision, for a reader allowed to see some Lairs. It commits nothing.
    #[must_use]
    pub fn retaining_lairs(mut self, mut keep: impl FnMut(LairId) -> bool) -> Self {
        self.lairs.retain(|id, _| keep(*id));
        self
    }

    fn validate_new_dojos(&self, dojos: &[Dojo]) -> Result<(), TopologyError> {
        let mut dojo_ids = HashSet::new();
        let mut splint_ids = HashSet::new();
        for dojo in dojos {
            validate_name(&dojo.name)?;
            if self.find_dojo(dojo.id).is_some() || !dojo_ids.insert(dojo.id) {
                return Err(TopologyError::DuplicateDojoId(dojo.id));
            }
            if dojo.root.find_splint(dojo.default_focus).is_none() {
                return Err(TopologyError::InvalidDojoDefaultFocus {
                    dojo_id: dojo.id,
                    splint_id: dojo.default_focus,
                });
            }
            collect_new_splint_ids(self, &dojo.root, &mut splint_ids)?;
        }
        Ok(())
    }

    fn check_revision(&self, expected: TopologyRevision) -> Result<(), TopologyError> {
        if expected != self.revision {
            return Err(TopologyError::StaleTopology {
                expected,
                current: self.revision,
            });
        }
        Ok(())
    }

    fn advance_revision(&mut self) {
        self.revision.0 = self
            .revision
            .0
            .checked_add(1)
            .expect("topology revision exhausted");
    }
}

fn normalized_name(value: &str) -> Result<String, TopologyError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(TopologyError::EmptyName);
    }
    if trimmed.len() > MAX_NAME_BYTES {
        return Err(TopologyError::NameTooLong);
    }
    Ok(trimmed.to_owned())
}

fn validate_name(value: &str) -> Result<(), TopologyError> {
    normalized_name(value).map(|_| ())
}

fn collect_new_splint_ids(
    topology: &Topology,
    node: &LayoutNode,
    ids: &mut HashSet<SplintId>,
) -> Result<(), TopologyError> {
    match node {
        LayoutNode::Leaf(splint) => {
            if topology.find_splint(splint.id).is_some() || !ids.insert(splint.id) {
                return Err(TopologyError::DuplicateSplintId(splint.id));
            }
        }
        LayoutNode::Branch { first, second, .. } => {
            collect_new_splint_ids(topology, first, ids)?;
            collect_new_splint_ids(topology, second, ids)?;
        }
    }
    Ok(())
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TopologyError {
    #[error("name cannot be empty")]
    EmptyName,
    #[error("name cannot exceed 128 UTF-8 bytes")]
    NameTooLong,
    #[error("a Lair named '{0}' already exists")]
    DuplicateLairName(String),
    #[error("Lair {0:?} does not exist")]
    LairNotFound(LairId),
    #[error("Lair {0:?} already exists")]
    DuplicateLairId(LairId),
    #[error("transient Lair {0:?} cannot be saved or pinned")]
    TransientLairRetention(LairId),
    #[error("Dojo {0:?} does not exist")]
    DojoNotFound(DojoId),
    #[error("default Dojo numbering is exhausted")]
    DefaultDojoNameExhausted,
    #[error("preset materialization requires at least one Dojo")]
    EmptyDojoSet,
    #[error("Dojo {0:?} already exists")]
    DuplicateDojoId(DojoId),
    #[error("Dojo {0:?} still contains a live Splint")]
    DojoStillLive(DojoId),
    #[error("Dojo {dojo_id:?} default focus references missing Splint {splint_id:?}")]
    InvalidDojoDefaultFocus {
        dojo_id: DojoId,
        splint_id: SplintId,
    },
    #[error("Splint {0:?} does not exist")]
    SplintNotFound(SplintId),
    #[error("Splint {0:?} already exists")]
    DuplicateSplintId(SplintId),
    #[error("Splint {0:?} is still live")]
    SplintStillLive(SplintId),
    #[error("Splint {0:?} has no parent split")]
    SplintHasNoParent(SplintId),
    #[error("topology revision is stale: expected {expected:?}, current {current:?}")]
    StaleTopology {
        expected: TopologyRevision,
        current: TopologyRevision,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_dojo_names_start_at_one_and_advance_past_the_highest_exact_number() {
        let mut lair = Lair::new("main", PathBuf::from("/tmp"));
        assert_eq!(lair.dojos[0].name, "Dojo 1");
        assert_eq!(lair.next_default_dojo_name().unwrap(), "Dojo 2");

        lair.dojos
            .push(Dojo::with_shell("Dojo 3", PathBuf::from("/tmp")));
        lair.dojos
            .push(Dojo::with_shell("logs", PathBuf::from("/tmp")));
        assert_eq!(lair.next_default_dojo_name().unwrap(), "Dojo 4");
    }

    #[test]
    fn legacy_and_empty_lairs_begin_numbering_at_one() {
        let mut lair = Lair::new("main", PathBuf::from("/tmp"));
        lair.dojos[0].name = "terminal".to_owned();
        assert_eq!(lair.next_default_dojo_name().unwrap(), "Dojo 1");

        lair.dojos.clear();
        assert_eq!(lair.next_default_dojo_name().unwrap(), "Dojo 1");
    }

    #[test]
    fn default_dojo_names_ignore_noncanonical_numbers_and_reject_exhaustion() {
        let mut lair = Lair::new("main", PathBuf::from("/tmp"));
        for name in [
            "dojo 9",
            "Dojo 0",
            "Dojo 02",
            "Dojo +8",
            "Dojo 7 ",
            "Dojo 18446744073709551616",
        ] {
            lair.dojos
                .push(Dojo::with_shell(name, PathBuf::from("/tmp")));
        }
        assert_eq!(lair.next_default_dojo_name().unwrap(), "Dojo 2");

        lair.dojos.push(Dojo::with_shell(
            format!("Dojo {}", u64::MAX),
            PathBuf::from("/tmp"),
        ));
        assert_eq!(
            lair.next_default_dojo_name(),
            Err(TopologyError::DefaultDojoNameExhausted)
        );
    }

    #[test]
    fn transient_lairs_also_start_with_dojo_one() {
        let lair = Lair::transient("private", PathBuf::from("/tmp"));
        assert_eq!(lair.dojos[0].name, "Dojo 1");
    }

    #[test]
    fn tab_organization_promotes_transient_lairs_in_one_revision() {
        let mut topology = Topology::new();
        let transient = Lair::transient("private", PathBuf::from("/tmp"));
        let lair_id = transient.id;
        let initial_dojo_id = transient.dojos[0].id;
        topology
            .insert_lair_at(TopologyRevision::default(), transient)
            .unwrap();

        let expected = topology.revision();
        let (revision, promoted) = topology
            .new_dojo_with_promotion_at(
                expected,
                lair_id,
                Dojo::with_shell("Dojo 2", PathBuf::from("/tmp")),
                true,
            )
            .unwrap();
        assert!(promoted);
        assert_eq!(revision.get(), expected.get() + 1);
        assert_eq!(
            topology.find_lair(lair_id).unwrap().lifetime,
            LairLifetime::Persistent
        );
        assert_eq!(topology.find_lair(lair_id).unwrap().dojos.len(), 2);

        let expected = topology.revision();
        let (revision, promoted) = topology
            .rename_dojo_with_promotion_at(expected, initial_dojo_id, "work", true)
            .unwrap();
        assert!(!promoted);
        assert_eq!(revision.get(), expected.get() + 1);
        assert_eq!(topology.find_dojo(initial_dojo_id).unwrap().name, "work");
    }

    #[test]
    fn failed_tab_organization_does_not_partially_promote() {
        let mut topology = Topology::new();
        let transient = Lair::transient("private", PathBuf::from("/tmp"));
        let lair_id = transient.id;
        let dojo_id = transient.dojos[0].id;
        topology
            .insert_lair_at(TopologyRevision::default(), transient)
            .unwrap();
        let stale = TopologyRevision::default();

        assert!(
            topology
                .rename_dojo_with_promotion_at(stale, dojo_id, "work", true)
                .is_err()
        );
        assert_eq!(
            topology.find_lair(lair_id).unwrap().lifetime,
            LairLifetime::Transient
        );
        assert_eq!(topology.find_dojo(dojo_id).unwrap().name, "Dojo 1");
    }

    #[test]
    fn runtime_state_can_transition_and_failed_creation_can_roll_back() {
        let mut topology = Topology::new();
        let lair = topology
            .create_lair("main", PathBuf::from("/tmp"))
            .unwrap()
            .clone();
        let splint_id = lair.dojos[0].root.find_splint_id_for_test();
        assert!(topology.set_splint_state(splint_id, SplintState::Running));
        assert_eq!(topology.remove_lair(lair.id).unwrap().id, lair.id);
        assert_eq!(topology.lairs().count(), 0);
    }

    #[test]
    fn atomic_lair_replacement_advances_once_and_preserves_bounds() {
        let mut topology = Topology::new();
        let removed = topology
            .create_lair("retired", PathBuf::from("/tmp"))
            .unwrap()
            .id;
        topology.create_lair("kept", PathBuf::from("/tmp")).unwrap();
        let expected = topology.revision();
        let inserted = Lair::new("fresh", PathBuf::from("/tmp"));
        let inserted_id = inserted.id;

        assert_eq!(
            topology
                .replace_lair_at(expected, removed, inserted)
                .unwrap(),
            TopologyRevision(expected.0 + 1)
        );
        assert_eq!(topology.lairs().count(), 2);
        assert!(topology.lairs().all(|lair| lair.id != removed));
        assert!(topology.lairs().any(|lair| lair.id == inserted_id));
    }

    #[test]
    fn lair_retention_is_revision_bound_and_rejects_transient_lairs() {
        let mut topology = Topology::new();
        let lair_id = topology
            .create_lair("main", PathBuf::from("/tmp"))
            .unwrap()
            .id;
        let stale = topology.revision();
        topology
            .set_lair_retention_at(stale, lair_id, LairRetention::Saved)
            .unwrap();
        assert_eq!(
            topology.find_lair(lair_id).unwrap().retention,
            LairRetention::Saved
        );
        assert!(matches!(
            topology.set_lair_retention_at(stale, lair_id, LairRetention::Pinned),
            Err(TopologyError::StaleTopology { .. })
        ));

        let transient = Lair::transient("private", PathBuf::from("/tmp"));
        let transient_id = transient.id;
        topology
            .insert_lair_at(topology.revision(), transient)
            .unwrap();
        assert_eq!(
            topology.set_lair_retention_at(topology.revision(), transient_id, LairRetention::Saved),
            Err(TopologyError::TransientLairRetention(transient_id))
        );
    }

    #[test]
    fn atomic_lair_termination_checks_revision_and_removes_exact_lair() {
        let mut topology = Topology::new();
        let lair_id = topology
            .create_lair("main", PathBuf::from("/tmp"))
            .unwrap()
            .id;
        let revision = topology.revision();
        assert_eq!(
            topology.terminate_lair_at(revision, lair_id).unwrap(),
            TopologyRevision(revision.0 + 1)
        );
        assert!(topology.lairs().all(|lair| lair.id != lair_id));
        assert!(matches!(
            topology.terminate_lair_at(revision, lair_id),
            Err(TopologyError::StaleTopology { .. })
        ));
    }

    #[test]
    fn a_lair_view_keeps_the_revision_and_only_the_selected_lairs() {
        let mut topology = Topology::new();
        let kept = topology
            .create_lair("kept", PathBuf::from("/tmp"))
            .unwrap()
            .id;
        topology
            .create_lair("hidden", PathBuf::from("/tmp"))
            .unwrap();
        let revision = topology.revision();
        let view = topology.retaining_lairs(|id| id == kept);
        assert_eq!(view.revision(), revision);
        let ids: Vec<_> = view.lairs().map(|lair| lair.id).collect();
        assert_eq!(ids, [kept]);
    }

    #[test]
    fn rejects_duplicate_lair_names_but_allows_duplicate_dojo_names() {
        let mut topology = Topology::new();
        let first = topology
            .create_lair(" main ", PathBuf::from("/tmp"))
            .unwrap()
            .id;
        assert_eq!(
            topology.create_lair("main", PathBuf::from("/tmp")),
            Err(TopologyError::DuplicateLairName("main".into()))
        );
        assert_eq!(
            topology.create_lair("x".repeat(129), PathBuf::from("/tmp")),
            Err(TopologyError::NameTooLong)
        );

        let duplicate_name = Dojo::with_shell("terminal", PathBuf::from("/var/tmp"));
        topology
            .new_dojo_at(topology.revision(), first, duplicate_name)
            .unwrap();
        assert_eq!(topology.find_lair(first).unwrap().dojos.len(), 2);
    }

    #[test]
    fn preset_materialization_commits_complete_dojo_sets_once() {
        let mut topology = Topology::new();
        let lair_id = topology
            .create_lair("main", PathBuf::from("/tmp"))
            .unwrap()
            .id;
        let before = topology.revision();
        let first = Dojo::with_shell("first", PathBuf::from("/tmp"));
        let first_id = first.id;
        let second = Dojo::with_shell("second", PathBuf::from("/var/tmp"));
        let second_id = second.id;
        assert_eq!(
            topology
                .materialize_dojos_at(before, lair_id, Some("renamed".into()), vec![first, second],)
                .unwrap()
                .get(),
            before.get() + 1
        );
        let lair = topology.find_lair(lair_id).unwrap();
        assert_eq!(lair.name, "renamed");
        assert!(lair.dojos.iter().any(|dojo| dojo.id == first_id));
        assert!(lair.dojos.iter().any(|dojo| dojo.id == second_id));

        let unchanged = topology.clone();
        let duplicate = unchanged.find_dojo(first_id).unwrap().clone();
        assert_eq!(
            topology.materialize_dojos_at(topology.revision(), lair_id, None, vec![duplicate]),
            Err(TopologyError::DuplicateDojoId(first_id))
        );
        assert_eq!(topology, unchanged);

        let revision = topology.revision();
        let dojo = Dojo::with_shell("new", PathBuf::from("/tmp"));
        let dojo_id = dojo.id;
        let (new_lair_id, committed) = topology
            .materialize_lair_at(revision, "new-lair", vec![dojo])
            .unwrap();
        assert_eq!(committed.get(), revision.get() + 1);
        assert_eq!(
            topology.find_lair(new_lair_id).unwrap().dojos[0].id,
            dojo_id
        );
    }

    #[test]
    fn split_and_close_preserve_ids_and_commit_once() {
        let mut topology = Topology::new();
        let lair_id = topology
            .create_lair("main", PathBuf::from("/tmp"))
            .unwrap()
            .id;
        let dojo_id = topology.lairs[&lair_id].dojos[0].id;
        let target_id = topology.lairs[&lair_id].dojos[0]
            .root
            .find_splint_id_for_test();
        let inserted = Splint::shell(PathBuf::from("/var/tmp"));
        let inserted_id = inserted.id;

        assert_eq!(
            topology
                .split_splint(
                    target_id,
                    inserted,
                    Axis::Vertical,
                    SplitSide::First,
                    SplitRatio::new(400).unwrap(),
                )
                .unwrap()
                .get(),
            2
        );
        let LayoutNode::Branch {
            axis,
            ratio,
            first,
            second,
        } = &topology.lairs[&lair_id].dojos[0].root
        else {
            panic!("split did not create a branch");
        };
        assert_eq!((*axis, ratio.get()), (Axis::Vertical, 400));
        assert_eq!(first.find_splint(inserted_id).unwrap().id, inserted_id);
        assert_eq!(second.find_splint(target_id).unwrap().id, target_id);

        let unchanged = topology.clone();
        assert_eq!(
            topology.close_splint(target_id),
            Err(TopologyError::SplintStillLive(target_id))
        );
        assert_eq!(topology, unchanged);

        assert!(topology.set_splint_state(target_id, SplintState::Exited(0)));
        assert_eq!(topology.close_splint(target_id).unwrap().get(), 3);
        let dojo = topology.find_dojo(dojo_id).unwrap();
        assert_eq!(dojo.root.find_splint(inserted_id).unwrap().id, inserted_id);
        assert_eq!(dojo.default_focus, inserted_id);

        assert!(topology.set_splint_state(inserted_id, SplintState::Exited(0)));
        assert_eq!(topology.close_splint(inserted_id).unwrap().get(), 4);
        assert!(topology.find_lair(lair_id).unwrap().dojos.is_empty());
        assert_eq!(topology.lairs().count(), 1);
    }

    #[test]
    fn renames_pin_the_title_and_a_reset_restores_the_default() {
        let mut topology = Topology::new();
        let lair = topology
            .create_lair("main", PathBuf::from("/tmp"))
            .unwrap()
            .clone();
        let id = lair.dojos[0].root.find_splint_id_for_test();
        assert!(topology.find_splint(id).unwrap().uses_default_title());

        let renamed = topology
            .rename_splint_at(topology.revision(), id, "editor")
            .unwrap();
        let splint = topology.find_splint(id).unwrap();
        assert_eq!((splint.title.as_str(), splint.title_set), ("editor", true));

        // Renaming to the default text pins it too.
        topology
            .rename_splint_at(renamed, id, crate::DEFAULT_SPLINT_TITLE)
            .unwrap();
        let splint = topology.find_splint(id).unwrap();
        assert!(splint.title_set && !splint.uses_default_title());

        let stale = renamed;
        let before = topology.clone();
        assert!(matches!(
            topology.reset_splint_title_at(stale, id),
            Err(TopologyError::StaleTopology { .. })
        ));
        assert_eq!(topology, before);

        let reset = topology
            .reset_splint_title_at(topology.revision(), id)
            .unwrap();
        assert!(reset > renamed);
        let splint = topology.find_splint(id).unwrap();
        assert_eq!(splint.title, crate::DEFAULT_SPLINT_TITLE);
        assert!(splint.uses_default_title());
        let missing = SplintId::new();
        assert_eq!(
            topology.reset_splint_title_at(topology.revision(), missing),
            Err(TopologyError::SplintNotFound(missing))
        );
    }

    #[test]
    fn relaunch_updates_exited_leaf_without_advancing_topology() {
        let mut topology = Topology::new();
        let lair = topology
            .create_lair("main", PathBuf::from("/tmp"))
            .unwrap()
            .clone();
        let id = lair.dojos[0].root.find_splint_id_for_test();
        let unchanged = topology.clone();
        assert_eq!(
            topology.commit_relaunch(id, PathBuf::from("/var/tmp"), vec!["echo".into()]),
            Err(TopologyError::SplintStillLive(id))
        );
        assert_eq!(topology, unchanged);

        assert!(topology.set_splint_state(id, SplintState::Exited(0)));
        let revision = topology.revision();
        topology
            .commit_relaunch(
                id,
                PathBuf::from("/var/tmp"),
                vec!["printf".into(), "ready".into()],
            )
            .unwrap();
        let splint = topology.find_splint(id).unwrap();
        assert_eq!(splint.cwd, PathBuf::from("/var/tmp"));
        assert_eq!(splint.command, vec!["printf", "ready"]);
        assert_eq!(splint.state, SplintState::Running);
        assert_eq!(topology.revision(), revision);
    }

    #[test]
    fn stale_revision_and_complete_tree_edits_are_transactional() {
        let mut topology = Topology::new();
        let lair = topology
            .create_lair("main", PathBuf::from("/tmp"))
            .unwrap()
            .clone();
        let lair_id = lair.id;
        let dojo_id = lair.dojos[0].id;
        let first_id = lair.dojos[0].root.find_splint_id_for_test();
        let base = topology.revision();
        let second = Splint::shell(PathBuf::from("/tmp"));
        let second_id = second.id;
        topology
            .split_splint_at(
                base,
                first_id,
                second,
                Axis::Horizontal,
                SplitSide::Second,
                SplitRatio::new(500).unwrap(),
            )
            .unwrap();
        let before = topology.clone();
        assert_eq!(
            topology.rename_splint_at(base, first_id, "stale"),
            Err(TopologyError::StaleTopology {
                expected: base,
                current: before.revision(),
            })
        );
        assert_eq!(topology, before);

        let revision = topology
            .set_split_ratio_at(
                topology.revision(),
                second_id,
                0,
                SplitRatio::new(650).unwrap(),
            )
            .unwrap();
        topology
            .rename_lair_at(revision, lair_id, "renamed")
            .unwrap();
        topology
            .rename_dojo_at(topology.revision(), dojo_id, "work")
            .unwrap();
        topology
            .set_dojo_default_focus_at(topology.revision(), dojo_id, second_id)
            .unwrap();
        assert_eq!(
            topology.find_dojo(dojo_id).unwrap().default_focus,
            second_id
        );
        let before = topology.clone();
        assert!(matches!(
            topology.set_dojo_default_focus_at(topology.revision(), dojo_id, SplintId::new()),
            Err(TopologyError::InvalidDojoDefaultFocus { .. })
        ));
        assert_eq!(topology, before);

        let extra = Dojo::with_shell("extra", PathBuf::from("/var/tmp"));
        let extra_id = extra.id;
        let extra_splint = extra.root.find_splint_id_for_test();
        topology
            .new_dojo_at(topology.revision(), lair_id, extra)
            .unwrap();
        assert_eq!(
            topology.close_dojo_at(topology.revision(), extra_id),
            Err(TopologyError::DojoStillLive(extra_id))
        );
        assert!(topology.set_splint_state(extra_splint, SplintState::Exited(0)));
        topology
            .close_dojo_at(topology.revision(), extra_id)
            .unwrap();
        assert!(topology.find_dojo(extra_id).is_none());
        assert_unique_valid_tree(&topology);

        // Termination removes a live Dojo, still at the expected revision only.
        let live = Dojo::with_shell("live", PathBuf::from("/var/tmp"));
        let live_id = live.id;
        topology
            .new_dojo_at(topology.revision(), lair_id, live)
            .unwrap();
        let stale = TopologyRevision::default();
        assert!(matches!(
            topology.terminate_dojo_at(stale, live_id),
            Err(TopologyError::StaleTopology { .. })
        ));
        let revision = topology.revision();
        let terminated = topology.terminate_dojo_at(revision, live_id).unwrap();
        assert!(terminated > revision);
        assert!(topology.find_dojo(live_id).is_none());
        assert_eq!(
            topology.terminate_dojo_at(terminated, live_id),
            Err(TopologyError::DojoNotFound(live_id))
        );
        assert_unique_valid_tree(&topology);
    }

    #[test]
    fn deterministic_edit_sequence_preserves_tree_invariants() {
        let mut topology = Topology::new();
        let lair = topology
            .create_lair("random", PathBuf::from("/tmp"))
            .unwrap()
            .clone();
        let mut ids = vec![lair.dojos[0].root.find_splint_id_for_test()];
        let mut seed = 0x5eed_u64;
        for index in 0..64 {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let target = ids[usize::try_from(seed).unwrap() % ids.len()];
            let splint = Splint::shell(PathBuf::from("/tmp"));
            ids.push(splint.id);
            let ratio = SplitRatio::new(u16::try_from(seed % 999 + 1).unwrap()).unwrap();
            topology
                .split_splint_at(
                    topology.revision(),
                    target,
                    splint,
                    if seed & 1 == 0 {
                        Axis::Horizontal
                    } else {
                        Axis::Vertical
                    },
                    if seed & 2 == 0 {
                        SplitSide::First
                    } else {
                        SplitSide::Second
                    },
                    ratio,
                )
                .unwrap();
            topology
                .rename_splint_at(topology.revision(), target, format!("pane-{index}"))
                .unwrap();
            assert_unique_valid_tree(&topology);
        }
    }

    #[test]
    fn failed_split_rolls_back_completely() {
        let mut topology = Topology::new();
        topology.create_lair("main", PathBuf::from("/tmp")).unwrap();
        let before = topology.clone();
        let missing = SplintId::new();

        assert_eq!(
            topology.split_splint(
                missing,
                Splint::shell(PathBuf::from("/tmp")),
                Axis::Horizontal,
                SplitSide::Second,
                SplitRatio::new(500).unwrap(),
            ),
            Err(TopologyError::SplintNotFound(missing))
        );
        assert_eq!(topology, before);
    }

    fn assert_unique_valid_tree(topology: &Topology) {
        fn visit(node: &LayoutNode, ids: &mut HashSet<SplintId>) {
            match node {
                LayoutNode::Leaf(splint) => assert!(ids.insert(splint.id)),
                LayoutNode::Branch {
                    ratio,
                    first,
                    second,
                    ..
                } => {
                    assert!((SplitRatio::MIN..=SplitRatio::MAX).contains(&ratio.get()));
                    visit(first, ids);
                    visit(second, ids);
                }
            }
        }
        let mut lair_ids = HashSet::new();
        let mut dojo_ids = HashSet::new();
        let mut splint_ids = HashSet::new();
        for lair in topology.lairs() {
            assert!(lair_ids.insert(lair.id));
            for dojo in &lair.dojos {
                assert!(dojo_ids.insert(dojo.id));
                assert!(dojo.root.find_splint(dojo.default_focus).is_some());
                visit(&dojo.root, &mut splint_ids);
            }
        }
    }

    trait TestLayoutExt {
        fn find_splint_id_for_test(&self) -> SplintId;
    }

    impl TestLayoutExt for LayoutNode {
        fn find_splint_id_for_test(&self) -> SplintId {
            match self {
                Self::Leaf(splint) => splint.id,
                Self::Branch { first, .. } => first.find_splint_id_for_test(),
            }
        }
    }
}
