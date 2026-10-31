#![forbid(unsafe_code)]
//! Renderer-independent terminal state for Splinterm.
//!
//! This crate begins a direct Rust port of Foot 1.27.0. The foundational
//! representations are derived from Foot's `terminal.h` at commit
//! `3c5b584b0eafa772eb4376fb6eaf6643399e190e`. Grid algorithms, parsing, PTY
//! ownership, rendering, and protocol serialization deliberately live outside
//! this layer. Borrowed semantic snapshots, revisions, and row-oriented damage
//! remain renderer- and protocol-independent.

mod cell;
mod composed;
mod config;
mod coord;
mod cursor;
mod event;
mod grid;
mod image;
mod mode;
mod row;
mod snapshot;
mod terminal;
mod update;
mod vt;

pub use cell::{Attributes, Cell, CellContent, Color, ColorSource, UnderlineStyle};
pub(crate) use composed::ComposedTable;
pub use config::{DEFAULT_SIXEL_PALETTE, SixelConfig, TerminalConfig};
pub use coord::{Coordinate, CoordinateRange, ScrollRegion};
pub use cursor::Cursor;
pub use event::{
    ClipboardSelection, OSC52_MAX_BASE64_BYTES, OSC52_MAX_BYTES, ReplyTerminator, TerminalEvent,
    osc52_reply,
};
pub use grid::{Grid, ScrollDirection, ScrollResult};
pub use image::{
    CellExtent, DEFAULT_KITTY_UPLOAD_BYTES_PER_DAEMON, ImageAlphaMode, ImageContent,
    ImageContentId, ImageContentMetadata, ImageErasePolicy, ImageError, ImageLimits, ImageMetrics,
    ImagePlacement, ImagePlacementId, ImagePlane, ImageRetention, ImageSourceFormat,
    NewImageContent, NewImagePlacement, NewImagePlacementOptions, PixelRect, PixelSize,
    SharedImageBudget, SharedImageBudgetMetrics, SharedKittyUploadBudget,
    SharedKittyUploadBudgetMetrics,
};
pub use mode::{ActiveScreen, MouseTracking, TerminalModes};
pub use row::Row;
pub use snapshot::{
    CellAttributesSnapshot, CellSnapshot, CellSnapshotContent, CursorSnapshot, Dimensions,
    RowSnapshot, ScrollbackPage, ScrollbackSnapshot, SearchFrom, SearchMatch, SearchPage,
    SnapshotRequest, TerminalSnapshot,
};
pub use terminal::{
    CheckpointError, MAX_IMAGE_STATE_CHECKPOINT_BYTES, MAX_TERMINAL_CHECKPOINT_BYTES,
    MAX_TERMINAL_STATE_CHECKPOINT_BYTES, TERMINAL_CHECKPOINT_SCHEMA_V1,
    TERMINAL_CHECKPOINT_SCHEMA_V2, Terminal,
};
pub(crate) use update::ChangeSet;
pub use update::{
    ResnapshotRequired, TerminalDamage, TerminalRevision, TerminalUpdate, UpdateBatch,
};
