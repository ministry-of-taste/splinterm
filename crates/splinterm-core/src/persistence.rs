use std::{
    collections::{BTreeMap, HashSet},
    path::Component,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    Dojo, DojoId, Lair, LairId, LairLifetime, LairRetention, LayoutNode, SplintId, SplintState,
    Topology, TopologyRevision,
};

const LEGACY_LAIR_SCHEMA_VERSION: u32 = 2;
const LEGACY_TOPOLOGY_SCHEMA_VERSION_V3: u32 = 3;
const LEGACY_TOPOLOGY_SCHEMA_VERSION_V4: u32 = 4;
pub const TOPOLOGY_SCHEMA_VERSION: u32 = 5;
pub const MAX_TOPOLOGY_DOCUMENT_BYTES: usize = 1024 * 1024;
pub const MAX_PERSISTENT_LAIRS: usize = 64;
pub const MAX_DOJOS_PER_LAIR: usize = 64;
pub const MAX_SPLINTS: usize = 256;
pub const MAX_LAYOUT_DEPTH: usize = 32;
const MAX_NAME_BYTES: usize = 128;
const MAX_ARGUMENTS: usize = 64;
const MAX_ARGUMENT_BYTES: usize = 4096;
const MAX_COMMAND_BYTES: usize = 32 * 1024;

/// Versioned metadata only; this document never represents live PTYs or processes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TopologyDocument {
    schema_version: u32,
    revision: TopologyRevision,
    lairs: Vec<Lair>,
}

#[derive(Deserialize)]
struct DocumentVersion {
    schema_version: u32,
}

/// Schema-v3 lacked explicit saved-layout retention and provenance.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyTopologyDocumentV3 {
    schema_version: u32,
    revision: TopologyRevision,
    lairs: Vec<LegacyLairV3>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyLairV3 {
    id: LairId,
    name: String,
    #[serde(default)]
    lifetime: LairLifetime,
    dojos: Vec<Dojo>,
}

impl LegacyTopologyDocumentV3 {
    fn migrate(self) -> Result<TopologyDocument, PersistenceError> {
        let mut lairs = self
            .lairs
            .into_iter()
            .map(|lair| Lair {
                id: lair.id,
                name: lair.name,
                lifetime: lair.lifetime,
                retention: LairRetention::Disposable,
                provenance: None,
                dojos: lair.dojos,
            })
            .collect::<Vec<_>>();
        normalize_legacy_default_dojo_names(&mut lairs)?;
        Ok(TopologyDocument {
            schema_version: TOPOLOGY_SCHEMA_VERSION,
            revision: self.revision,
            lairs,
        })
    }
}

/// Schema-v4 could persist generated fallback names for unnamed Dojos.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyTopologyDocumentV4 {
    schema_version: u32,
    revision: TopologyRevision,
    lairs: Vec<Lair>,
}

impl LegacyTopologyDocumentV4 {
    fn migrate(mut self) -> Result<TopologyDocument, PersistenceError> {
        normalize_legacy_default_dojo_names(&mut self.lairs)?;
        Ok(TopologyDocument {
            schema_version: TOPOLOGY_SCHEMA_VERSION,
            revision: self.revision,
            lairs: self.lairs,
        })
    }
}

/// Exact schema-v2 representation used only for validated migration.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyLairDocumentV2 {
    schema_version: u32,
    revision: TopologyRevision,
    dojos: Vec<LegacyDojoV2>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyDojoV2 {
    id: Uuid,
    name: String,
    windows: Vec<LegacyWindowV2>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyWindowV2 {
    id: Uuid,
    title: String,
    default_focus: SplintId,
    root: LayoutNode,
}

impl LegacyLairDocumentV2 {
    fn migrate(self) -> Result<TopologyDocument, PersistenceError> {
        let mut lairs = self
            .dojos
            .into_iter()
            .map(|legacy_lair| Lair {
                id: LairId::from_uuid(legacy_lair.id),
                name: legacy_lair.name,
                lifetime: LairLifetime::Persistent,
                retention: LairRetention::Disposable,
                provenance: None,
                dojos: legacy_lair
                    .windows
                    .into_iter()
                    .map(|legacy_dojo| Dojo {
                        id: DojoId::from_uuid(legacy_dojo.id),
                        name: legacy_dojo.title,
                        default_focus: legacy_dojo.default_focus,
                        root: legacy_dojo.root,
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        normalize_legacy_default_dojo_names(&mut lairs)?;
        Ok(TopologyDocument {
            schema_version: TOPOLOGY_SCHEMA_VERSION,
            revision: self.revision,
            lairs,
        })
    }
}

fn is_decimal_digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_legacy_default_dojo_name(name: &str) -> bool {
    if name == "terminal" {
        return true;
    }
    let Some(suffix) = name.strip_prefix("terminal-") else {
        return false;
    };
    let mut components = suffix.split('-');
    let Some(timestamp) = components.next() else {
        return false;
    };
    let process_id = components.next();
    components.next().is_none()
        && is_decimal_digits(timestamp)
        && process_id.is_none_or(is_decimal_digits)
}

fn normalize_legacy_default_dojo_names(lairs: &mut [Lair]) -> Result<(), PersistenceError> {
    for lair in lairs {
        for index in 0..lair.dojos.len() {
            if is_legacy_default_dojo_name(&lair.dojos[index].name) {
                lair.dojos[index].name = lair
                    .next_default_dojo_name()
                    .map_err(|_| PersistenceError::DefaultDojoNameExhausted)?;
            }
        }
    }
    Ok(())
}

impl TopologyDocument {
    /// Creates and validates a metadata snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when the model cannot be represented as safe durable metadata.
    pub fn from_topology(topology: &Topology) -> Result<Self, PersistenceError> {
        let mut lairs: Vec<_> = topology
            .lairs
            .values()
            .filter(|lair| lair.lifetime.is_persistent())
            .cloned()
            .collect();
        for lair in &mut lairs {
            for dojo in &mut lair.dojos {
                mark_tree_restorable(&mut dojo.root);
            }
        }
        let document = Self {
            schema_version: TOPOLOGY_SCHEMA_VERSION,
            revision: topology.revision,
            lairs,
        };
        document.validate()?;
        Ok(document)
    }

    /// Decodes and validates current metadata or migrates supported legacy schemas.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed, unsupported, unsafe, or oversized metadata.
    pub fn decode(bytes: &[u8]) -> Result<Self, PersistenceError> {
        if bytes.len() > MAX_TOPOLOGY_DOCUMENT_BYTES {
            return Err(PersistenceError::DocumentTooLarge);
        }
        let version: DocumentVersion = serde_json::from_slice(bytes)
            .map_err(|error| PersistenceError::Decode(error.to_string()))?;
        let document = match version.schema_version {
            TOPOLOGY_SCHEMA_VERSION => serde_json::from_slice(bytes)
                .map_err(|error| PersistenceError::Decode(error.to_string()))?,
            LEGACY_TOPOLOGY_SCHEMA_VERSION_V4 => {
                let legacy: LegacyTopologyDocumentV4 = serde_json::from_slice(bytes)
                    .map_err(|error| PersistenceError::Decode(error.to_string()))?;
                if legacy.schema_version != LEGACY_TOPOLOGY_SCHEMA_VERSION_V4 {
                    return Err(PersistenceError::UnsupportedVersion(legacy.schema_version));
                }
                legacy.migrate()?
            }
            LEGACY_TOPOLOGY_SCHEMA_VERSION_V3 => {
                let legacy: LegacyTopologyDocumentV3 = serde_json::from_slice(bytes)
                    .map_err(|error| PersistenceError::Decode(error.to_string()))?;
                if legacy.schema_version != LEGACY_TOPOLOGY_SCHEMA_VERSION_V3 {
                    return Err(PersistenceError::UnsupportedVersion(legacy.schema_version));
                }
                legacy.migrate()?
            }
            LEGACY_LAIR_SCHEMA_VERSION => {
                let legacy: LegacyLairDocumentV2 = serde_json::from_slice(bytes)
                    .map_err(|error| PersistenceError::Decode(error.to_string()))?;
                if legacy.schema_version != LEGACY_LAIR_SCHEMA_VERSION {
                    return Err(PersistenceError::UnsupportedVersion(legacy.schema_version));
                }
                legacy.migrate()?
            }
            other => return Err(PersistenceError::UnsupportedVersion(other)),
        };
        document.validate()?;
        Ok(document)
    }

    /// Serializes validated current-schema metadata.
    ///
    /// # Errors
    ///
    /// Returns an error when validation or serialization fails.
    pub fn encode(&self) -> Result<Vec<u8>, PersistenceError> {
        self.validate()?;
        serde_json::to_vec(self).map_err(|error| PersistenceError::Encode(error.to_string()))
    }

    /// Converts validated metadata into an in-memory Topology.
    ///
    /// # Errors
    ///
    /// Returns an error if this document has become invalid.
    pub fn into_topology(self) -> Result<Topology, PersistenceError> {
        self.validate()?;
        Ok(Topology {
            revision: self.revision,
            lairs: self.lair_map(),
        })
    }

    fn lair_map(self) -> BTreeMap<LairId, Lair> {
        self.lairs
            .into_iter()
            .filter(|lair| lair.lifetime.is_persistent())
            .map(|lair| (lair.id, lair))
            .collect()
    }

    fn validate(&self) -> Result<(), PersistenceError> {
        if self.schema_version != TOPOLOGY_SCHEMA_VERSION {
            return Err(PersistenceError::UnsupportedVersion(self.schema_version));
        }
        if self.lairs.len() > MAX_PERSISTENT_LAIRS {
            return Err(PersistenceError::CollectionTooLarge("Lairs"));
        }

        let mut lair_ids = HashSet::new();
        let mut lair_names = HashSet::new();
        let mut dojo_ids = HashSet::new();
        let mut splint_ids = HashSet::new();
        let mut splint_count = 0;
        for lair in &self.lairs {
            validate_name(&lair.name, "Lair name")?;
            if let Some(provenance) = &lair.provenance {
                validate_name(provenance, "Lair provenance")?;
            }
            if !lair.lifetime.is_persistent()
                && (lair.retention.is_protected() || lair.provenance.is_some())
            {
                return Err(PersistenceError::InvalidLairLifecycle);
            }
            if !lair_ids.insert(lair.id) {
                return Err(PersistenceError::DuplicateId("Lair"));
            }
            if !lair_names.insert(lair.name.as_str()) {
                return Err(PersistenceError::DuplicateLairName);
            }
            if lair.dojos.len() > MAX_DOJOS_PER_LAIR {
                return Err(PersistenceError::CollectionTooLarge("Dojos"));
            }
            for dojo in &lair.dojos {
                validate_name(&dojo.name, "Dojo name")?;
                if !dojo_ids.insert(dojo.id) {
                    return Err(PersistenceError::DuplicateId("Dojo"));
                }
                if dojo.root.find_splint(dojo.default_focus).is_none() {
                    return Err(PersistenceError::InvalidDojoDefaultFocus);
                }
                validate_tree(&dojo.root, 1, &mut splint_count, &mut splint_ids)?;
            }
        }
        Ok(())
    }
}

fn mark_tree_restorable(node: &mut LayoutNode) {
    match node {
        LayoutNode::Leaf(splint) => {
            if !matches!(splint.state, SplintState::Exited(_)) {
                splint.state = SplintState::Exited(0);
            }
        }
        LayoutNode::Branch { first, second, .. } => {
            mark_tree_restorable(first);
            mark_tree_restorable(second);
        }
    }
}

fn validate_tree(
    node: &LayoutNode,
    depth: usize,
    count: &mut usize,
    ids: &mut HashSet<SplintId>,
) -> Result<(), PersistenceError> {
    if depth > MAX_LAYOUT_DEPTH {
        return Err(PersistenceError::LayoutTooDeep);
    }
    match node {
        LayoutNode::Leaf(splint) => {
            *count += 1;
            if *count > MAX_SPLINTS {
                return Err(PersistenceError::CollectionTooLarge("Splints"));
            }
            if !ids.insert(splint.id) {
                return Err(PersistenceError::DuplicateId("Splint"));
            }
            validate_name(&splint.title, "Splint title")?;
            validate_cwd(&splint.cwd)?;
            validate_command(&splint.command)?;
            if splint.launch.shell.as_ref().is_some_and(|shell| {
                shell.is_empty()
                    || shell.len() > MAX_ARGUMENT_BYTES
                    || shell.as_bytes().contains(&0)
            }) || splint.launch.scrollback_lines > 1_000_000
                || !(1..=4096).contains(&splint.launch.columns)
                || !(1..=4096).contains(&splint.launch.rows)
                || splint.last_incarnation == Some(0)
            {
                return Err(PersistenceError::InvalidLaunchMetadata);
            }
            if !matches!(splint.state, SplintState::Exited(_)) {
                return Err(PersistenceError::LiveProcessState);
            }
        }
        LayoutNode::Branch { first, second, .. } => {
            validate_tree(first, depth + 1, count, ids)?;
            validate_tree(second, depth + 1, count, ids)?;
        }
    }
    Ok(())
}

fn validate_name(value: &str, kind: &'static str) -> Result<(), PersistenceError> {
    if value.trim().is_empty() || value.trim() != value || value.len() > MAX_NAME_BYTES {
        return Err(PersistenceError::InvalidName(kind));
    }
    Ok(())
}

fn validate_cwd(path: &std::path::Path) -> Result<(), PersistenceError> {
    if path.to_str().is_none()
        || !path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return Err(PersistenceError::UnsafeWorkingDirectory);
    }
    Ok(())
}

fn validate_command(command: &[String]) -> Result<(), PersistenceError> {
    if command.len() > MAX_ARGUMENTS
        || command.iter().any(|arg| {
            arg.is_empty() || arg.len() > MAX_ARGUMENT_BYTES || arg.as_bytes().contains(&0)
        })
        || command.iter().map(String::len).sum::<usize>() > MAX_COMMAND_BYTES
    {
        return Err(PersistenceError::InvalidCommand);
    }
    Ok(())
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PersistenceError {
    #[error("metadata document exceeds its size limit")]
    DocumentTooLarge,
    #[error("metadata schema version {0} is unsupported")]
    UnsupportedVersion(u32),
    #[error("metadata decoding failed: {0}")]
    Decode(String),
    #[error("metadata encoding failed: {0}")]
    Encode(String),
    #[error("metadata contains too many {0}")]
    CollectionTooLarge(&'static str),
    #[error("metadata contains a duplicate {0} ID")]
    DuplicateId(&'static str),
    #[error("metadata cannot allocate a numbered name for a legacy unnamed Dojo")]
    DefaultDojoNameExhausted,
    #[error("metadata contains duplicate Lair names")]
    DuplicateLairName,
    #[error("metadata contains an invalid {0}")]
    InvalidName(&'static str),
    #[error("metadata layout exceeds its depth limit")]
    LayoutTooDeep,
    #[error("metadata contains an unsafe working directory")]
    UnsafeWorkingDirectory,
    #[error("metadata contains invalid launch arguments")]
    InvalidCommand,
    #[error("metadata contains invalid launch policy or geometry")]
    InvalidLaunchMetadata,
    #[error("metadata Dojo default focus does not reference its own layout")]
    InvalidDojoDefaultFocus,
    #[error("metadata claims a process is live")]
    LiveProcessState,
    #[error("metadata contains an invalid Lair lifecycle")]
    InvalidLairLifecycle,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use uuid::Uuid;

    use super::*;

    const LAIR_ID: &str = "018f4d8c-2a18-4b31-8c2f-9e7c5de77101";
    const DOJO_ID: &str = "018f4d8c-2a18-4b31-8c2f-9e7c5de77102";
    const SECOND_DOJO_ID: &str = "018f4d8c-2a18-4b31-8c2f-9e7c5de77103";
    const SPLINT_ID: &str = "018f4d8c-2a18-4b31-8c2f-9e7c5de77110";
    const SECOND_SPLINT_ID: &str = "018f4d8c-2a18-4b31-8c2f-9e7c5de77111";

    fn leaf(id: &str, cwd: &str) -> Value {
        json!({"Leaf": {
            "id": id,
            "title": "shell",
            "cwd": cwd,
            "command": [],
            "state": {"Exited": 0}
        }})
    }

    fn dojo(name: &str) -> Value {
        let splint_id = Uuid::new_v4();
        json!({
            "id": Uuid::new_v4(),
            "name": name,
            "default_focus": splint_id,
            "root": leaf(&splint_id.to_string(), "/tmp")
        })
    }

    fn valid_v3_document() -> Value {
        json!({
            "schema_version": TOPOLOGY_SCHEMA_VERSION,
            "revision": 7,
            "lairs": [{
                "id": LAIR_ID,
                "name": "main",
                "dojos": [{
                    "id": DOJO_ID,
                    "name": "terminal",
                    "default_focus": SPLINT_ID,
                    "root": leaf(SPLINT_ID, "/tmp")
                }]
            }]
        })
    }

    fn valid_v2_document() -> Value {
        json!({
            "schema_version": LEGACY_LAIR_SCHEMA_VERSION,
            "revision": 7,
            "dojos": [{
                "id": LAIR_ID,
                "name": "main",
                "windows": [{
                    "id": DOJO_ID,
                    "title": "terminal",
                    "default_focus": SPLINT_ID,
                    "root": leaf(SPLINT_ID, "/tmp")
                }, {
                    "id": SECOND_DOJO_ID,
                    "title": "logs",
                    "default_focus": SECOND_SPLINT_ID,
                    "root": leaf(SECOND_SPLINT_ID, "/var/tmp")
                }]
            }]
        })
    }

    fn decode(value: &Value) -> Result<TopologyDocument, PersistenceError> {
        TopologyDocument::decode(&serde_json::to_vec(value).unwrap())
    }

    #[test]
    fn live_model_is_serialized_only_as_restorable_metadata() {
        let mut topology = Topology::new();
        let lair = topology
            .create_lair("main", std::path::PathBuf::from("/tmp"))
            .unwrap()
            .clone();
        let LayoutNode::Leaf(created) = &lair.dojos[0].root else {
            unreachable!()
        };
        assert!(topology.set_splint_last_incarnation(created.id, 41));
        let document = TopologyDocument::from_topology(&topology).unwrap();
        let restored = TopologyDocument::decode(&document.encode().unwrap())
            .unwrap()
            .into_topology()
            .unwrap();
        let LayoutNode::Leaf(splint) = &restored.lairs().next().unwrap().dojos[0].root else {
            unreachable!()
        };
        assert_eq!(splint.state, SplintState::Exited(0));
        assert_eq!(splint.last_incarnation, Some(41));
    }

    #[test]
    fn durable_projection_omits_transient_lairs_and_default_lifetime_field() {
        let mut topology = Topology::new();
        let persistent_id = topology
            .create_lair("persistent", std::path::PathBuf::from("/tmp"))
            .unwrap()
            .id;
        let transient = Lair::transient("transient", std::path::PathBuf::from("/var/tmp"));
        let transient_id = transient.id;
        topology
            .insert_lair_at(topology.revision(), transient)
            .unwrap();

        let encoded = TopologyDocument::from_topology(&topology)
            .unwrap()
            .encode()
            .unwrap();
        let value: Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(value["revision"], json!(2));
        assert_eq!(value["lairs"].as_array().unwrap().len(), 1);
        assert_eq!(value["lairs"][0]["id"], json!(persistent_id));
        assert!(value["lairs"][0].get("lifetime").is_none());

        let restored = TopologyDocument::decode(&encoded)
            .unwrap()
            .into_topology()
            .unwrap();
        assert!(restored.lairs().any(|lair| lair.id == persistent_id));
        assert!(!restored.lairs().any(|lair| lair.id == transient_id));
    }

    #[test]
    fn explicitly_tagged_transient_metadata_is_discarded_on_restore() {
        let mut value = valid_v3_document();
        value["lairs"][0]["lifetime"] = json!("transient");
        let restored = decode(&value).unwrap().into_topology().unwrap();
        assert_eq!(restored.revision().get(), 7);
        assert_eq!(restored.lairs().count(), 0);
    }

    #[test]
    fn independent_dojo_trees_and_focus_hints_round_trip() {
        let mut topology = Topology::new();
        let lair_id = topology
            .create_lair("main", std::path::PathBuf::from("/tmp"))
            .unwrap()
            .id;
        let first_dojo = topology.lairs[&lair_id].dojos[0].id;
        let first_hint = topology.lairs[&lair_id].dojos[0].default_focus;
        let second = Dojo::with_shell("logs", std::path::PathBuf::from("/var/tmp"));
        let second_dojo = second.id;
        let second_hint = second.default_focus;
        topology
            .new_dojo_at(topology.revision(), lair_id, second)
            .unwrap();
        let sibling = crate::Splint::shell(std::path::PathBuf::from("/tmp"));
        let sibling_id = sibling.id;
        topology
            .split_splint_at(
                topology.revision(),
                first_hint,
                sibling,
                crate::Axis::Horizontal,
                crate::SplitSide::Second,
                crate::SplitRatio::new(600).unwrap(),
            )
            .unwrap();
        topology
            .set_dojo_default_focus_at(topology.revision(), first_dojo, sibling_id)
            .unwrap();

        let restored = TopologyDocument::decode(
            &TopologyDocument::from_topology(&topology)
                .unwrap()
                .encode()
                .unwrap(),
        )
        .unwrap()
        .into_topology()
        .unwrap();
        assert_eq!(
            restored.find_dojo(first_dojo).unwrap().root.splint_count(),
            2
        );
        assert_eq!(
            restored.find_dojo(first_dojo).unwrap().default_focus,
            sibling_id
        );
        assert_eq!(
            restored.find_dojo(second_dojo).unwrap().root.splint_count(),
            1
        );
        assert_eq!(
            restored.find_dojo(second_dojo).unwrap().default_focus,
            second_hint
        );
    }

    #[test]
    fn schema_v3_migration_defaults_lifecycle_and_normalizes_legacy_dojo_names() {
        let mut value = valid_v3_document();
        value["schema_version"] = json!(LEGACY_TOPOLOGY_SCHEMA_VERSION_V3);
        let topology = decode(&value).unwrap().into_topology().unwrap();
        let lair = topology.lairs().next().unwrap();
        assert_eq!(lair.retention, LairRetention::Disposable);
        assert_eq!(lair.provenance, None);
        assert_eq!(lair.dojos[0].name, "Dojo 1");
    }

    #[test]
    fn schema_v4_migration_replaces_only_numeric_generated_fallback_names() {
        let mut value = valid_v3_document();
        value["schema_version"] = json!(LEGACY_TOPOLOGY_SCHEMA_VERSION_V4);
        let dojos = value["lairs"][0]["dojos"].as_array_mut().unwrap();
        dojos.extend([
            dojo("terminal-123"),
            dojo("terminal-123-45"),
            dojo("Dojo 3"),
            dojo("terminal-012"),
            dojo("terminal-000-007"),
            dojo("terminal-123-worker"),
            dojo("my-terminal-123"),
        ]);

        let topology = decode(&value).unwrap().into_topology().unwrap();
        let names = topology
            .lairs()
            .next()
            .unwrap()
            .dojos
            .iter()
            .map(|dojo| dojo.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "Dojo 4",
                "Dojo 5",
                "Dojo 6",
                "Dojo 3",
                "Dojo 7",
                "Dojo 8",
                "terminal-123-worker",
                "my-terminal-123",
            ]
        );
    }

    #[test]
    fn legacy_generated_name_migration_rejects_number_exhaustion() {
        let mut value = valid_v3_document();
        value["schema_version"] = json!(LEGACY_TOPOLOGY_SCHEMA_VERSION_V4);
        value["lairs"][0]["dojos"]
            .as_array_mut()
            .unwrap()
            .push(dojo(&format!("Dojo {}", u64::MAX)));

        assert_eq!(
            decode(&value),
            Err(PersistenceError::DefaultDojoNameExhausted)
        );
    }

    #[test]
    fn saved_lifecycle_and_provenance_round_trip() {
        let mut topology = Topology::new();
        let lair_id = topology
            .create_lair("main", std::path::PathBuf::from("/tmp"))
            .unwrap()
            .id;
        topology
            .set_lair_retention_at(topology.revision(), lair_id, LairRetention::Pinned)
            .unwrap();
        topology.lairs.get_mut(&lair_id).unwrap().provenance = Some("preset: tdl".into());
        let restored = TopologyDocument::decode(
            &TopologyDocument::from_topology(&topology)
                .unwrap()
                .encode()
                .unwrap(),
        )
        .unwrap()
        .into_topology()
        .unwrap();
        let lair = restored.find_lair(lair_id).unwrap();
        assert_eq!(lair.retention, LairRetention::Pinned);
        assert_eq!(lair.provenance.as_deref(), Some("preset: tdl"));
    }

    #[test]
    fn schema_v2_migration_preserves_identity_and_layout_boundaries() {
        let document = decode(&valid_v2_document()).unwrap();
        let topology = document.into_topology().unwrap();
        assert_eq!(topology.revision().get(), 7);
        let lair = topology.lairs().next().unwrap();
        assert_eq!(lair.id.to_string(), LAIR_ID);
        assert_eq!(lair.name, "main");
        assert_eq!(lair.dojos.len(), 2);
        assert_eq!(lair.dojos[0].id.to_string(), DOJO_ID);
        assert_eq!(lair.dojos[0].name, "Dojo 1");
        assert_eq!(lair.dojos[0].default_focus.to_string(), SPLINT_ID);
        assert_eq!(lair.dojos[1].id.to_string(), SECOND_DOJO_ID);
        assert_eq!(lair.dojos[1].name, "logs");
        assert_eq!(lair.dojos[1].default_focus.to_string(), SECOND_SPLINT_ID);
    }

    #[test]
    fn migrated_schema_encodes_only_as_current_version() {
        let document = decode(&valid_v2_document()).unwrap();
        let encoded: Value = serde_json::from_slice(&document.encode().unwrap()).unwrap();
        assert_eq!(encoded["schema_version"], json!(TOPOLOGY_SCHEMA_VERSION));
        assert!(encoded.get("lairs").is_some());
        assert!(encoded.get("dojos").is_none());
    }

    #[test]
    fn accepts_current_version_exited_metadata_without_rewriting_explicit_names() {
        let document = decode(&valid_v3_document()).unwrap();
        let encoded = document.encode().unwrap();
        let topology = TopologyDocument::decode(&encoded)
            .unwrap()
            .into_topology()
            .unwrap();
        assert_eq!(topology.revision().get(), 7);
        assert_eq!(topology.lairs().count(), 1);
        assert_eq!(topology.lairs().next().unwrap().dojos[0].name, "terminal");
    }

    // Plan 0069: `title_set` is additive. A document saved before it existed
    // keeps every renamed Splint named; only the default title follows apps.
    #[test]
    fn saved_titles_load_as_set_unless_default_and_a_pinned_default_round_trips() {
        let mut value = valid_v3_document();
        let second = json!({
            "id": SECOND_DOJO_ID,
            "name": "logs",
            "default_focus": SECOND_SPLINT_ID,
            "root": leaf(SECOND_SPLINT_ID, "/var/tmp")
        });
        value["lairs"][0]["dojos"]
            .as_array_mut()
            .unwrap()
            .push(second);
        value["lairs"][0]["dojos"][1]["root"]["Leaf"]["title"] = json!("logs");
        let topology = decode(&value).unwrap().into_topology().unwrap();
        let first_id: SplintId = SPLINT_ID.parse().unwrap();
        let second_id: SplintId = SECOND_SPLINT_ID.parse().unwrap();
        assert!(topology.find_splint(first_id).unwrap().uses_default_title());
        let renamed = topology.find_splint(second_id).unwrap();
        assert!(renamed.title_set && renamed.title == "logs");

        let legacy = decode(&valid_v2_document())
            .unwrap()
            .into_topology()
            .unwrap();
        assert!(legacy.find_splint(first_id).unwrap().uses_default_title());

        let mut pinned = topology;
        pinned
            .rename_splint_at(pinned.revision(), first_id, crate::DEFAULT_SPLINT_TITLE)
            .unwrap();
        let encoded = TopologyDocument::from_topology(&pinned)
            .unwrap()
            .encode()
            .unwrap();
        let restored = TopologyDocument::decode(&encoded)
            .unwrap()
            .into_topology()
            .unwrap();
        let splint = restored.find_splint(first_id).unwrap();
        assert_eq!(splint.title, crate::DEFAULT_SPLINT_TITLE);
        assert!(splint.title_set);
    }

    #[test]
    fn rejects_unknown_version_running_state_and_unsafe_path() {
        let mut value = valid_v3_document();
        value["schema_version"] = json!(TOPOLOGY_SCHEMA_VERSION + 1);
        assert_eq!(
            decode(&value),
            Err(PersistenceError::UnsupportedVersion(
                TOPOLOGY_SCHEMA_VERSION + 1
            ))
        );

        let mut value = valid_v3_document();
        value["lairs"][0]["dojos"][0]["root"]["Leaf"]["state"] = json!("Running");
        assert_eq!(decode(&value), Err(PersistenceError::LiveProcessState));

        let mut value = valid_v2_document();
        value["dojos"][0]["windows"][0]["root"]["Leaf"]["cwd"] = json!("../tmp");
        assert_eq!(
            decode(&value),
            Err(PersistenceError::UnsafeWorkingDirectory)
        );
    }

    #[test]
    fn rejects_invalid_dojo_focus_hint() {
        let mut value = valid_v3_document();
        value["lairs"][0]["dojos"][0]["default_focus"] = json!(Uuid::new_v4());
        assert_eq!(
            decode(&value),
            Err(PersistenceError::InvalidDojoDefaultFocus)
        );
    }

    #[test]
    fn rejects_duplicate_ids_invalid_ratios_and_oversized_collections() {
        let mut value = valid_v3_document();
        let duplicate = value["lairs"][0].clone();
        value["lairs"].as_array_mut().unwrap().push(duplicate);
        assert_eq!(decode(&value), Err(PersistenceError::DuplicateId("Lair")));

        let mut value = valid_v3_document();
        let leaf = value["lairs"][0]["dojos"][0]["root"].clone();
        value["lairs"][0]["dojos"][0]["root"] = json!({
            "Branch": {"axis": "Horizontal", "ratio": 0, "first": leaf.clone(), "second": leaf}
        });
        assert!(matches!(decode(&value), Err(PersistenceError::Decode(_))));

        let mut value = valid_v3_document();
        let lair = value["lairs"][0].clone();
        value["lairs"] = Value::Array(vec![lair; MAX_PERSISTENT_LAIRS + 1]);
        assert_eq!(
            decode(&value),
            Err(PersistenceError::CollectionTooLarge("Lairs"))
        );
    }

    #[test]
    fn rejects_unknown_document_fields_in_v2_and_v3() {
        let mut current = valid_v3_document();
        current["unexpected"] = json!(true);
        assert!(matches!(decode(&current), Err(PersistenceError::Decode(_))));

        let mut legacy = valid_v2_document();
        legacy["dojos"][0]["unexpected"] = json!(true);
        assert!(matches!(decode(&legacy), Err(PersistenceError::Decode(_))));
    }

    #[test]
    fn rejects_oversized_documents_before_parsing() {
        assert_eq!(
            TopologyDocument::decode(&vec![b' '; MAX_TOPOLOGY_DOCUMENT_BYTES + 1]),
            Err(PersistenceError::DocumentTooLarge)
        );
    }
}
