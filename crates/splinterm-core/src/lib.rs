//! Persistent session and layout primitives shared by the client and daemon.

mod layout;
mod model;
mod persistence;

pub use layout::{
    Axis, DEFAULT_SPLINT_TITLE, LayoutNode, Splint, SplintId, SplintLaunchMetadata, SplintState,
    SplitRatio, SplitSide,
};
pub use model::{
    Dojo, DojoId, Lair, LairId, LairLifetime, LairRetention, Topology, TopologyError,
    TopologyRevision, generated_lair_name,
};
pub use persistence::{
    MAX_DOJOS_PER_LAIR, MAX_LAYOUT_DEPTH, MAX_PERSISTENT_LAIRS, MAX_SPLINTS,
    MAX_TOPOLOGY_DOCUMENT_BYTES, PersistenceError, TOPOLOGY_SCHEMA_VERSION, TopologyDocument,
};
