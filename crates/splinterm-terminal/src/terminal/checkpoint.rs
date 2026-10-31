//! Canonical, bounded terminal checkpoint schemas.
//!
//! Schema v1 owns terminal/parser state only. Schema v2 adds completed image
//! contents, placements, and Kitty application-image mappings as one separately
//! digested component restored atomically with the grids. Descriptor storage,
//! sealing, actor write queues, PTY state, and active image transfers belong to
//! later handoff boundaries.

use std::{
    fmt,
    io::{self, Write},
    marker::PhantomData,
};

use serde::{
    Deserialize, Deserializer, Serialize,
    de::{SeqAccess, Visitor},
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    ImageContentId, SharedImageBudget,
    grid::MAX_CHECKPOINT_GRID_ROWS,
    image::{ImageCheckpointV1, MAX_IMAGE_CHECKPOINT_CONTENTS},
    row::MAX_CHECKPOINT_CELLS,
};

use super::{
    ActiveScreen, Attributes, ChangeSet, ComposedTable, Grid, ImagePlane, ImageRetention,
    ImageSourceFormat, KittyImageIds, MAX_SIXEL_COLORS, Parser, ScrollRegion,
    SharedKittyUploadBudget, SixelPaletteMode, Terminal, TerminalConfig, TerminalEvent,
    TerminalModes, TerminalRevision,
};

pub const TERMINAL_CHECKPOINT_SCHEMA_V1: u16 = 1;
pub const TERMINAL_CHECKPOINT_SCHEMA_V2: u16 = 2;
/// Bound for the terminal component: grids, parser and pending events.
pub const MAX_TERMINAL_STATE_CHECKPOINT_BYTES: usize = 64 * 1024 * 1024;
/// Bound for the image component. Pixels are base64, so this covers the
/// default 32 MiB per-terminal image budget plus catalog metadata. Images get
/// their own allowance so they never consume the terminal's.
pub const MAX_IMAGE_STATE_CHECKPOINT_BYTES: usize = 48 * 1024 * 1024;
/// Aggregate bound: both components plus the envelope.
pub const MAX_TERMINAL_CHECKPOINT_BYTES: usize =
    MAX_TERMINAL_STATE_CHECKPOINT_BYTES + MAX_IMAGE_STATE_CHECKPOINT_BYTES + 64 * 1024;
const CHECKPOINT_MAGIC_V1: [u8; 8] = *b"SPLTCP01";
const CHECKPOINT_MAGIC_V2: [u8; 8] = *b"SPLTCP02";
const MAX_CHECKPOINT_TITLE_BYTES: usize = 64 * 1024;
const MAX_CHECKPOINT_EVENT_BODY_BYTES: usize = 1024 * 1024;
const MAX_CHECKPOINT_EVENTS: usize = 1 << 16;

#[derive(Debug, Error)]
pub enum CheckpointError {
    #[error("terminal checkpoint exceeds {MAX_TERMINAL_CHECKPOINT_BYTES} bytes")]
    TooLarge,
    #[error("terminal checkpoint encoding is malformed: {0}")]
    Encoding(#[source] serde_json::Error),
    #[error("terminal checkpoint encoding is not canonical")]
    NonCanonical,
    #[error("terminal checkpoint configuration does not match the restore authority")]
    IncompatibleConfig,
    #[error("terminal state is not representable by checkpoint schema v1")]
    UnsupportedState,
    #[error("terminal checkpoint state violates terminal invariants")]
    InvalidState,
}

#[derive(Deserialize)]
struct CheckpointHeader {
    magic: [u8; 8],
    schema: u16,
    body: CheckpointBodyShape,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CheckpointEnvelopeV1 {
    magic: [u8; 8],
    schema: u16,
    body_sha256: [u8; 32],
    body: TerminalCheckpointV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CheckpointEnvelopeV2 {
    magic: [u8; 8],
    schema: u16,
    body_sha256: [u8; 32],
    body: TerminalCheckpointV2,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct TerminalCheckpointV2 {
    terminal: TerminalCheckpointV1,
    image_sha256: [u8; 32],
    images: ImageStateCheckpointV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ImageStateCheckpointV1 {
    plane: ImageCheckpointV1,
    kitty_image_ids: KittyImageIdsCheckpointV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct KittyImageIdsCheckpointV1 {
    #[serde(deserialize_with = "deserialize_kitty_image_ids")]
    normal: Vec<KittyImageIdCheckpoint>,
    #[serde(deserialize_with = "deserialize_kitty_image_ids")]
    alternate: Vec<KittyImageIdCheckpoint>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct KittyImageIdCheckpoint {
    application_id: u32,
    content_id: ImageContentId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct TerminalCheckpointV1 {
    config: ConfigEvidence,
    normal: Grid,
    alternate: Grid,
    active: ActiveScreen,
    parser: Parser,
    attributes: Attributes,
    saved_attributes: Attributes,
    scroll_region: ScrollRegion,
    modes: TerminalModes,
    #[serde(deserialize_with = "deserialize_tab_stops")]
    tab_stops: Vec<bool>,
    composed: ComposedTable,
    cell_pixels: Option<(u32, u32)>,
    sixel_scrolling: bool,
    sixel_cursor_right: bool,
    sixel_palette_mode: SixelPaletteMode,
    #[serde(deserialize_with = "deserialize_sixel_palette")]
    sixel_shared_palette: Vec<u32>,
    sixel_palette_size: usize,
    sixel_maximum_width: u32,
    sixel_maximum_height: u32,
    title: String,
    #[serde(deserialize_with = "deserialize_palette")]
    palette: Vec<u32>,
    #[serde(deserialize_with = "deserialize_palette")]
    initial_palette: Vec<u32>,
    default_colors: [u32; 3],
    initial_default_colors: [u32; 3],
    #[serde(deserialize_with = "deserialize_events")]
    events: Vec<CheckpointEvent>,
    revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ConfigEvidence {
    scrollback_lines: usize,
    tab_width: usize,
    osc_limit: usize,
    dcs_limit: usize,
    composed_limit: usize,
    event_limit: usize,
    update_history_limit: usize,
    image_bytes_per_content: usize,
    image_bytes_per_terminal: usize,
    image_contents_per_terminal: usize,
    image_placements_per_terminal: usize,
    image_maximum_dimension: u32,
    image_maximum_pixels: usize,
    shared_image_budget_limit: Option<usize>,
    shared_kitty_upload_budget_limit: Option<usize>,
    sixel_enabled: bool,
    sixel_private_palette: bool,
    sixel_palette: [u32; 16],
}

impl ConfigEvidence {
    fn from_config(config: &TerminalConfig) -> Self {
        // Exhaustive destructuring: a new configuration field must be added
        // here, or drift in it would restore silently.
        let TerminalConfig {
            scrollback_lines,
            tab_width,
            osc_limit,
            dcs_limit,
            composed_limit,
            event_limit,
            update_history_limit,
            image_limits:
                crate::ImageLimits {
                    bytes_per_content,
                    bytes_per_terminal,
                    contents_per_terminal,
                    placements_per_terminal,
                    maximum_dimension,
                    maximum_pixels,
                },
            shared_image_budget,
            shared_kitty_upload_budget,
            sixel:
                crate::SixelConfig {
                    enabled: sixel_enabled,
                    private_palette: sixel_private_palette,
                    palette: sixel_palette,
                },
        } = config;
        Self {
            scrollback_lines: *scrollback_lines,
            tab_width: *tab_width,
            osc_limit: *osc_limit,
            dcs_limit: *dcs_limit,
            composed_limit: *composed_limit,
            event_limit: *event_limit,
            update_history_limit: *update_history_limit,
            image_bytes_per_content: *bytes_per_content,
            image_bytes_per_terminal: *bytes_per_terminal,
            image_contents_per_terminal: *contents_per_terminal,
            image_placements_per_terminal: *placements_per_terminal,
            image_maximum_dimension: *maximum_dimension,
            image_maximum_pixels: *maximum_pixels,
            shared_image_budget_limit: shared_image_budget.as_ref().map(SharedImageBudget::limit),
            shared_kitty_upload_budget_limit: shared_kitty_upload_budget
                .as_ref()
                .map(SharedKittyUploadBudget::limit),
            sixel_enabled: *sixel_enabled,
            sixel_private_palette: *sixel_private_palette,
            sixel_palette: *sixel_palette,
        }
    }

    fn matches(&self, config: &TerminalConfig) -> bool {
        *self == Self::from_config(config)
    }

    /// Reconstructs the exact encoded configuration around the receiving
    /// generation's process-wide budget handles.
    ///
    /// Shared budgets are runtime handles, so only their limits are evidence;
    /// both must be present and equal to the supplied handles' limits.
    fn handoff_config(
        &self,
        shared_image_budget: SharedImageBudget,
        shared_kitty_upload_budget: SharedKittyUploadBudget,
    ) -> Result<TerminalConfig, CheckpointError> {
        // Exhaustive destructuring: a new evidence field must be restored here.
        let Self {
            scrollback_lines,
            tab_width,
            osc_limit,
            dcs_limit,
            composed_limit,
            event_limit,
            update_history_limit,
            image_bytes_per_content,
            image_bytes_per_terminal,
            image_contents_per_terminal,
            image_placements_per_terminal,
            image_maximum_dimension,
            image_maximum_pixels,
            shared_image_budget_limit,
            shared_kitty_upload_budget_limit,
            sixel_enabled,
            sixel_private_palette,
            sixel_palette,
        } = self;
        if *shared_image_budget_limit != Some(shared_image_budget.limit())
            || *shared_kitty_upload_budget_limit != Some(shared_kitty_upload_budget.limit())
        {
            return Err(CheckpointError::IncompatibleConfig);
        }
        let config = TerminalConfig {
            scrollback_lines: *scrollback_lines,
            tab_width: *tab_width,
            osc_limit: *osc_limit,
            dcs_limit: *dcs_limit,
            composed_limit: *composed_limit,
            event_limit: *event_limit,
            update_history_limit: *update_history_limit,
            image_limits: crate::ImageLimits {
                bytes_per_content: *image_bytes_per_content,
                bytes_per_terminal: *image_bytes_per_terminal,
                contents_per_terminal: *image_contents_per_terminal,
                placements_per_terminal: *image_placements_per_terminal,
                maximum_dimension: *image_maximum_dimension,
                maximum_pixels: *image_maximum_pixels,
            },
            shared_image_budget: Some(shared_image_budget),
            shared_kitty_upload_budget: Some(shared_kitty_upload_budget),
            sixel: crate::SixelConfig {
                enabled: *sixel_enabled,
                private_palette: *sixel_private_palette,
                palette: *sixel_palette,
            },
        };
        if !self.matches(&config) {
            return Err(CheckpointError::IncompatibleConfig);
        }
        Ok(config)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
enum CheckpointEvent {
    Bell,
    PtyWrite(#[serde(deserialize_with = "deserialize_event_bytes")] Vec<u8>),
    TitleChanged(String),
    PaletteChanged { index: u16, color: u32 },
}

impl CheckpointEvent {
    fn from_terminal(event: &TerminalEvent) -> Result<Self, CheckpointError> {
        match event {
            TerminalEvent::Bell => Ok(Self::Bell),
            TerminalEvent::PtyWrite(bytes) => Ok(Self::PtyWrite(bytes.clone())),
            TerminalEvent::TitleChanged(title) => Ok(Self::TitleChanged(title.clone())),
            TerminalEvent::PaletteChanged { index, color } => Ok(Self::PaletteChanged {
                index: *index,
                color: *color,
            }),
            TerminalEvent::UnsupportedSequence(_)
            | TerminalEvent::ImageRejected(_)
            | TerminalEvent::StringTruncated(_)
            | TerminalEvent::EventQueueOverflow
            | TerminalEvent::ClipboardSet { .. }
            | TerminalEvent::ClipboardQuery { .. } => Err(CheckpointError::UnsupportedState),
        }
    }

    fn into_terminal(self) -> TerminalEvent {
        match self {
            Self::Bell => TerminalEvent::Bell,
            Self::PtyWrite(bytes) => TerminalEvent::PtyWrite(bytes),
            Self::TitleChanged(title) => TerminalEvent::TitleChanged(title),
            Self::PaletteChanged { index, color } => TerminalEvent::PaletteChanged { index, color },
        }
    }

    fn body_bytes(&self) -> usize {
        match self {
            Self::PtyWrite(bytes) => bytes.len(),
            Self::TitleChanged(title) => title.len(),
            Self::Bell | Self::PaletteChanged { .. } => 0,
        }
    }
}

impl KittyImageIdsCheckpointV1 {
    /// Captures both screens' mappings after proving they satisfy the same
    /// invariants restore enforces against the captured image plane.
    fn capture(ids: &KittyImageIds, images: &ImagePlane) -> Result<Self, CheckpointError> {
        let capture = |screen: ActiveScreen,
                       entries: &std::collections::BTreeMap<u32, ImageContentId>|
         -> Result<Vec<KittyImageIdCheckpoint>, CheckpointError> {
            let entries = entries
                .iter()
                .map(|(application_id, content_id)| KittyImageIdCheckpoint {
                    application_id: *application_id,
                    content_id: *content_id,
                })
                .collect::<Vec<_>>();
            validate_kitty_image_ids(images, screen, &entries)?;
            Ok(entries)
        };
        Ok(Self {
            normal: capture(ActiveScreen::Normal, &ids.normal)?,
            alternate: capture(ActiveScreen::Alternate, &ids.alternate)?,
        })
    }

    fn restore(self, images: &ImagePlane) -> Result<KittyImageIds, CheckpointError> {
        let restore = |screen: ActiveScreen,
                       entries: Vec<KittyImageIdCheckpoint>|
         -> Result<
            std::collections::BTreeMap<u32, ImageContentId>,
            CheckpointError,
        > {
            validate_kitty_image_ids(images, screen, &entries)?;
            Ok(entries
                .into_iter()
                .map(|entry| (entry.application_id, entry.content_id))
                .collect())
        };
        Ok(KittyImageIds {
            normal: restore(ActiveScreen::Normal, self.normal)?,
            alternate: restore(ActiveScreen::Alternate, self.alternate)?,
        })
    }
}

/// One screen's Kitty mappings must use strictly increasing non-zero
/// application ids, each naming distinct explicitly retained Kitty content on
/// that screen. An alias would survive deletion of the image through its other
/// id, and a mapping to absent content names nothing.
fn validate_kitty_image_ids(
    images: &ImagePlane,
    screen: ActiveScreen,
    entries: &[KittyImageIdCheckpoint],
) -> Result<(), CheckpointError> {
    let mut contents = std::collections::BTreeSet::new();
    let mut previous_application_id = None;
    for entry in entries {
        let is_kitty_content = images
            .content(screen, entry.content_id)
            .is_some_and(|content| {
                content.metadata().retention == ImageRetention::ExplicitDelete
                    && matches!(
                        content.metadata().source_format,
                        ImageSourceFormat::KittyRgb
                            | ImageSourceFormat::KittyRgba
                            | ImageSourceFormat::KittyPng
                    )
            });
        if entry.application_id == 0
            || previous_application_id.is_some_and(|previous| previous >= entry.application_id)
            || !is_kitty_content
            || !contents.insert(entry.content_id)
        {
            return Err(CheckpointError::InvalidState);
        }
        previous_application_id = Some(entry.application_id);
    }
    Ok(())
}

impl TerminalCheckpointV2 {
    fn capture(terminal: &Terminal) -> Result<Self, CheckpointError> {
        // Refuse an oversized image catalog before anything is copied.
        if !terminal
            .images
            .checkpoint_may_fit(MAX_IMAGE_STATE_CHECKPOINT_BYTES)
        {
            return Err(CheckpointError::TooLarge);
        }
        let terminal_body = TerminalCheckpointV1::capture(terminal)?;
        let normal_row_ids = terminal.normal.retained_row_ids();
        let alternate_row_ids = terminal.alternate.retained_row_ids();
        let images = ImageStateCheckpointV1 {
            plane: terminal
                .images
                .capture_checkpoint(&normal_row_ids, &alternate_row_ids)
                .map_err(|_| CheckpointError::InvalidState)?,
            kitty_image_ids: KittyImageIdsCheckpointV1::capture(
                &terminal.kitty_image_ids,
                &terminal.images,
            )?,
        };
        digest_json(&terminal_body, MAX_TERMINAL_STATE_CHECKPOINT_BYTES)?;
        let image_sha256 = digest_json(&images, MAX_IMAGE_STATE_CHECKPOINT_BYTES)?;
        Ok(Self {
            terminal: terminal_body,
            image_sha256,
            images,
        })
    }

    fn restore(self, config: TerminalConfig) -> Result<Terminal, CheckpointError> {
        self.terminal.restore(config, Some(self.images))
    }
}

impl TerminalCheckpointV1 {
    fn capture(terminal: &Terminal) -> Result<Self, CheckpointError> {
        if terminal.synchronized_updates
            || !terminal.synchronized_change.is_empty()
            || terminal.current_change.is_some()
            || terminal.sixel_decoder.is_some()
            || terminal.kitty_chunk.is_some()
            || terminal.kitty_upload.is_some()
            || terminal.iterm_transfer.is_some()
            || terminal.event_overflowed
            || terminal
                .normal
                .allocated_cells()
                .checked_add(terminal.alternate.allocated_cells())
                .is_none_or(|cells| cells > MAX_CHECKPOINT_CELLS)
        {
            return Err(CheckpointError::UnsupportedState);
        }
        let events = terminal
            .events
            .iter()
            .map(CheckpointEvent::from_terminal)
            .collect::<Result<Vec<_>, _>>()?;
        let mut composed = terminal.composed.clone();
        let parser = terminal.parser.checkpoint_copy();
        let event_body_bytes = events.iter().try_fold(0_usize, |total, event| {
            total.checked_add(event.body_bytes())
        });
        if terminal.title.len() > MAX_CHECKPOINT_TITLE_BYTES
            || events.len() > terminal.config.event_limit
            || events.len() > MAX_CHECKPOINT_EVENTS
            || event_body_bytes.is_none_or(|bytes| bytes > MAX_CHECKPOINT_EVENT_BODY_BYTES)
            || events.iter().any(
                |event| matches!(event, CheckpointEvent::TitleChanged(title) if title.len() > MAX_CHECKPOINT_TITLE_BYTES),
            )
            || !terminal.attributes.checkpoint_is_valid()
            || !terminal.saved_attributes.checkpoint_is_valid()
            || !composed.validate_and_rebuild_checkpoint(terminal.config.composed_limit)
            || !parser.checkpoint_is_valid(terminal.config.osc_limit, terminal.config.dcs_limit)
            || !terminal
                .normal
                .checkpoint_is_valid(terminal.composed.checkpoint_entry_count())
            || !terminal
                .alternate
                .checkpoint_is_valid(terminal.composed.checkpoint_entry_count())
        {
            return Err(CheckpointError::InvalidState);
        }
        Ok(Self {
            config: ConfigEvidence::from_config(&terminal.config),
            normal: terminal.normal.clone(),
            alternate: terminal.alternate.clone(),
            active: terminal.active,
            parser,
            attributes: terminal.attributes,
            saved_attributes: terminal.saved_attributes,
            scroll_region: terminal.scroll_region,
            modes: terminal.modes,
            tab_stops: terminal.tab_stops.clone(),
            composed,
            cell_pixels: terminal.cell_pixels,
            sixel_scrolling: terminal.sixel_scrolling,
            sixel_cursor_right: terminal.sixel_cursor_right,
            sixel_palette_mode: terminal.sixel_palette_mode,
            sixel_shared_palette: terminal.sixel_shared_palette.to_vec(),
            sixel_palette_size: terminal.sixel_palette_size,
            sixel_maximum_width: terminal.sixel_maximum_width,
            sixel_maximum_height: terminal.sixel_maximum_height,
            title: terminal.title.clone(),
            palette: terminal.palette.to_vec(),
            initial_palette: terminal.initial_palette.to_vec(),
            default_colors: terminal.default_colors,
            initial_default_colors: terminal.initial_default_colors,
            events,
            revision: terminal.revision.value(),
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "all-or-nothing validation and construction keep decoded checkpoint state local"
    )]
    fn restore(
        mut self,
        config: TerminalConfig,
        image_state: Option<ImageStateCheckpointV1>,
    ) -> Result<Terminal, CheckpointError> {
        if !self.config.matches(&config) {
            return Err(CheckpointError::IncompatibleConfig);
        }
        if !self
            .composed
            .validate_and_rebuild_checkpoint(config.composed_limit)
        {
            return Err(CheckpointError::InvalidState);
        }
        let composed_entries = self.composed.checkpoint_entry_count();
        let columns = self.normal.columns();
        let rows = self.normal.screen_rows();
        let expected_normal_capacity = config
            .scrollback_lines
            .checked_add(rows)
            .map(|requested| requested.max(rows))
            .and_then(usize::checked_next_power_of_two)
            .map(|capacity| capacity.min(1_usize << 30));
        let expected_alternate_capacity = rows
            .checked_next_power_of_two()
            .map(|capacity| capacity.min(1_usize << 30));
        let scroll_start = self.scroll_region.start();
        let scroll_end = self.scroll_region.end();
        let event_body_bytes = self.events.iter().try_fold(0_usize, |total, event| {
            total.checked_add(event.body_bytes())
        });
        if !self.normal.checkpoint_is_valid(composed_entries)
            || !self.alternate.checkpoint_is_valid(composed_entries)
            || self.alternate.columns() != columns
            || self.alternate.screen_rows() != rows
            || expected_normal_capacity != Some(self.normal.row_capacity())
            || expected_alternate_capacity != Some(self.alternate.row_capacity())
            || !self
                .parser
                .checkpoint_is_valid(config.osc_limit, config.dcs_limit)
            || !self.attributes.checkpoint_is_valid()
            || !self.saved_attributes.checkpoint_is_valid()
            || self.tab_stops.len() != columns
            || scroll_start < 0
            || scroll_start >= scroll_end
            || usize::try_from(scroll_end).map_or(true, |end| end > rows)
            || self.title.len() > MAX_CHECKPOINT_TITLE_BYTES
            || self.events.len() > config.event_limit
            || self.events.len() > MAX_CHECKPOINT_EVENTS
            || event_body_bytes.is_none_or(|bytes| bytes > MAX_CHECKPOINT_EVENT_BODY_BYTES)
            || self
                .events
                .iter()
                .any(|event| matches!(event, CheckpointEvent::TitleChanged(title) if title.len() > MAX_CHECKPOINT_TITLE_BYTES))
            || self
                .cell_pixels
                .is_some_and(|(width, height)| width == 0 || height == 0)
            || self.sixel_shared_palette.len() != MAX_SIXEL_COLORS
            || self.palette.len() != 256
            || self.initial_palette.len() != 256
            || !(2..=MAX_SIXEL_COLORS).contains(&self.sixel_palette_size)
            || self.sixel_maximum_width > config.image_limits.maximum_dimension
            || self.sixel_maximum_height > config.image_limits.maximum_dimension
            || self.revision > crate::grid::CHECKPOINT_COUNTER_HEADROOM
        {
            return Err(CheckpointError::InvalidState);
        }

        let sixel_shared_palette = self
            .sixel_shared_palette
            .into_boxed_slice()
            .try_into()
            .map_err(|_| CheckpointError::InvalidState)?;
        let palette = self
            .palette
            .try_into()
            .map_err(|_| CheckpointError::InvalidState)?;
        let initial_palette = self
            .initial_palette
            .try_into()
            .map_err(|_| CheckpointError::InvalidState)?;
        let (images, kitty_image_ids) = if let Some(image_state) = image_state {
            let images = ImagePlane::restore_checkpoint(
                image_state.plane,
                config.image_limits,
                config.shared_image_budget.clone(),
                &self.normal.retained_row_ids(),
                &self.alternate.retained_row_ids(),
            )
            .map_err(|_| CheckpointError::InvalidState)?;
            let kitty_image_ids = image_state.kitty_image_ids.restore(&images)?;
            (images, kitty_image_ids)
        } else {
            (
                ImagePlane::new_with_shared_budget(
                    config.image_limits,
                    config.shared_image_budget.clone(),
                ),
                KittyImageIds::default(),
            )
        };
        let kitty_upload_budget = config
            .shared_kitty_upload_budget
            .clone()
            .unwrap_or_else(|| {
                SharedKittyUploadBudget::new(crate::image::DEFAULT_KITTY_UPLOAD_BYTES_PER_DAEMON)
            });
        Ok(Terminal {
            normal: self.normal,
            alternate: self.alternate,
            active: self.active,
            parser: self.parser,
            config,
            attributes: self.attributes,
            saved_attributes: self.saved_attributes,
            scroll_region: self.scroll_region,
            modes: self.modes,
            synchronized_updates: false,
            synchronized_change: ChangeSet::default(),
            tab_stops: self.tab_stops,
            composed: self.composed,
            images,
            sixel_decoder: None,
            kitty_chunk: None,
            kitty_upload: None,
            iterm_transfer: None,
            kitty_upload_budget,
            kitty_image_ids,
            cell_pixels: self.cell_pixels,
            sixel_scrolling: self.sixel_scrolling,
            sixel_cursor_right: self.sixel_cursor_right,
            sixel_palette_mode: self.sixel_palette_mode,
            sixel_shared_palette,
            sixel_palette_size: self.sixel_palette_size,
            sixel_maximum_width: self.sixel_maximum_width,
            sixel_maximum_height: self.sixel_maximum_height,
            title: self.title,
            palette,
            initial_palette,
            default_colors: self.default_colors,
            initial_default_colors: self.initial_default_colors,
            events: self
                .events
                .into_iter()
                .map(CheckpointEvent::into_terminal)
                .collect(),
            event_overflowed: false,
            revision: TerminalRevision::new(self.revision),
            update_history: std::collections::VecDeque::new(),
            current_change: None,
        })
    }
}

enum DecodedCheckpoint {
    V1(TerminalCheckpointV1),
    V2(TerminalCheckpointV2),
}

impl DecodedCheckpoint {
    const fn schema(&self) -> u16 {
        match self {
            Self::V1(_) => TERMINAL_CHECKPOINT_SCHEMA_V1,
            Self::V2(_) => TERMINAL_CHECKPOINT_SCHEMA_V2,
        }
    }

    const fn config(&self) -> &ConfigEvidence {
        match self {
            Self::V1(body) => &body.config,
            Self::V2(body) => &body.terminal.config,
        }
    }

    fn restore(self, config: TerminalConfig) -> Result<Terminal, CheckpointError> {
        match self {
            Self::V1(body) => body.restore(config, None),
            Self::V2(body) => body.restore(config),
        }
    }
}

impl Terminal {
    /// Encodes the terminal into canonical checkpoint schema v2 bytes.
    ///
    /// # Errors
    /// Rejects active transactions/transfers, unsupported pending events,
    /// invalid state, or an aggregate larger than the hard byte bound.
    pub fn encode_checkpoint(&self) -> Result<Vec<u8>, CheckpointError> {
        let body = TerminalCheckpointV2::capture(self)?;
        let envelope = CheckpointEnvelopeV2 {
            magic: CHECKPOINT_MAGIC_V2,
            schema: TERMINAL_CHECKPOINT_SCHEMA_V2,
            body_sha256: digest_json(&body, MAX_TERMINAL_CHECKPOINT_BYTES)?,
            body,
        };
        encode_json(&envelope)
    }

    /// Validates canonical checkpoint framing and digests without constructing a terminal.
    ///
    /// This proves the closed encoding and returns its schema. Configuration-bound
    /// semantic validation still occurs in [`Self::restore_checkpoint`].
    ///
    /// # Errors
    /// Rejects oversized, malformed, noncanonical, unknown-schema, or digest-invalid bytes.
    pub fn validate_checkpoint_encoding(bytes: &[u8]) -> Result<u16, CheckpointError> {
        Ok(decode_checkpoint(bytes)?.schema())
    }

    /// Restores canonical checkpoint schema v1 or v2 under the supplied configuration.
    ///
    /// # Errors
    /// Rejects oversized, malformed, noncanonical, incompatible, unsupported,
    /// or internally invalid state without publishing a partial terminal.
    pub fn restore_checkpoint(
        bytes: &[u8],
        config: TerminalConfig,
    ) -> Result<Self, CheckpointError> {
        decode_checkpoint(bytes)?.restore(config)
    }

    /// Restores a transient handoff checkpoint using its sealed configuration
    /// evidence and the receiving generation's exact process-wide budgets.
    ///
    /// The checkpoint remains authority for per-terminal parser, image, and
    /// resource limits. Shared budget identities are reconstructed runtime
    /// handles whose limits must exactly match the encoded evidence.
    ///
    /// # Errors
    /// Rejects malformed state, absent shared-budget evidence, or any
    /// shared-budget limit mismatch before publishing a partial terminal.
    pub fn restore_handoff_checkpoint(
        bytes: &[u8],
        shared_image_budget: SharedImageBudget,
        shared_kitty_upload_budget: SharedKittyUploadBudget,
    ) -> Result<Self, CheckpointError> {
        let checkpoint = decode_checkpoint(bytes)?;
        let config = checkpoint
            .config()
            .handoff_config(shared_image_budget, shared_kitty_upload_budget)?;
        checkpoint.restore(config)
    }
}

fn decode_checkpoint(bytes: &[u8]) -> Result<DecodedCheckpoint, CheckpointError> {
    if bytes.len() > MAX_TERMINAL_CHECKPOINT_BYTES {
        return Err(CheckpointError::TooLarge);
    }
    let header = decode_header(bytes)?;
    match (header.magic, header.schema) {
        (CHECKPOINT_MAGIC_V1, TERMINAL_CHECKPOINT_SCHEMA_V1) => {
            let envelope: CheckpointEnvelopeV1 =
                serde_json::from_slice(bytes).map_err(CheckpointError::Encoding)?;
            if encode_json(&envelope)? != bytes {
                return Err(CheckpointError::NonCanonical);
            }
            if digest_json(&envelope.body, MAX_TERMINAL_STATE_CHECKPOINT_BYTES)?
                != envelope.body_sha256
            {
                return Err(CheckpointError::InvalidState);
            }
            Ok(DecodedCheckpoint::V1(envelope.body))
        }
        (CHECKPOINT_MAGIC_V2, TERMINAL_CHECKPOINT_SCHEMA_V2) => {
            let envelope: CheckpointEnvelopeV2 =
                serde_json::from_slice(bytes).map_err(CheckpointError::Encoding)?;
            if encode_json(&envelope)? != bytes {
                return Err(CheckpointError::NonCanonical);
            }
            // Each component has its own bound, so images never consume the
            // terminal's allowance.
            digest_json(&envelope.body.terminal, MAX_TERMINAL_STATE_CHECKPOINT_BYTES)?;
            if digest_json(&envelope.body, MAX_TERMINAL_CHECKPOINT_BYTES)? != envelope.body_sha256
                || digest_json(&envelope.body.images, MAX_IMAGE_STATE_CHECKPOINT_BYTES)?
                    != envelope.body.image_sha256
            {
                return Err(CheckpointError::InvalidState);
            }
            Ok(DecodedCheckpoint::V2(envelope.body))
        }
        _ => Err(CheckpointError::InvalidState),
    }
}

/// Hashes a value's canonical encoding without materializing it, failing past
/// `limit` bytes.
fn digest_json(value: &impl Serialize, limit: usize) -> Result<[u8; 32], CheckpointError> {
    let mut writer = DigestWriter {
        hasher: Sha256::new(),
        length: 0,
        limit,
        exceeded: false,
    };
    match serde_json::to_writer(&mut writer, value) {
        Ok(()) => Ok(writer.hasher.finalize().into()),
        Err(_) if writer.exceeded => Err(CheckpointError::TooLarge),
        Err(error) => Err(CheckpointError::Encoding(error)),
    }
}

struct DigestWriter {
    hasher: Sha256,
    length: usize,
    limit: usize,
    exceeded: bool,
}

impl Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self.length.checked_add(bytes.len()) {
            Some(next) if next <= self.limit => self.length = next,
            _ => {
                self.exceeded = true;
                return Err(io::Error::other("checkpoint component exceeds its bound"));
            }
        }
        self.hasher.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode_json(value: &impl Serialize) -> Result<Vec<u8>, CheckpointError> {
    let mut writer = BoundedWriter::default();
    match serde_json::to_writer(&mut writer, value) {
        Ok(()) => Ok(writer.bytes),
        Err(_) if writer.exceeded => Err(CheckpointError::TooLarge),
        Err(error) => Err(CheckpointError::Encoding(error)),
    }
}

fn deserialize_kitty_image_ids<'de, D>(
    deserializer: D,
) -> Result<Vec<KittyImageIdCheckpoint>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_sequence(
        deserializer,
        MAX_IMAGE_CHECKPOINT_CONTENTS,
        "Kitty application image mappings",
    )
}

/// Reads the envelope schema and sums every allocated row's declared width
/// without decoding any cell.
///
/// A packed row stores its trailing run once, so the full decode would expand
/// a few bytes into a whole row. This pass reads only the grid rows' widths,
/// so a checkpoint past [`MAX_CHECKPOINT_CELLS`] fails before any expansion.
fn decode_header(bytes: &[u8]) -> Result<CheckpointHeader, CheckpointError> {
    let header: CheckpointHeader =
        serde_json::from_slice(bytes).map_err(CheckpointError::Encoding)?;
    header
        .body
        .declared_cells()
        .filter(|cells| *cells <= MAX_CHECKPOINT_CELLS)
        .ok_or_else(|| CheckpointError::Encoding(cell_budget_error()))?;
    Ok(header)
}

fn cell_budget_error<E: serde::de::Error>() -> E {
    E::custom("terminal checkpoint rows exceed the aggregate cell budget")
}

/// The body fields of either schema that size its grids; every other field,
/// including each row's packed cells, is skipped without allocation.
#[derive(Deserialize)]
struct CheckpointBodyShape {
    normal: Option<GridShape>,
    alternate: Option<GridShape>,
    /// Schema v2 nests the v1 body, and so its grids, under `terminal`.
    terminal: Option<Box<CheckpointBodyShape>>,
}

impl CheckpointBodyShape {
    fn declared_cells(&self) -> Option<usize> {
        let nested = self
            .terminal
            .as_ref()
            .map_or(Some(0), |terminal| terminal.declared_cells())?;
        [&self.normal, &self.alternate]
            .into_iter()
            .flatten()
            .try_fold(nested, |total, grid| total.checked_add(grid.rows.0))
    }
}

#[derive(Deserialize)]
struct GridShape {
    rows: DeclaredRowCells,
}

#[derive(Deserialize)]
struct RowShape {
    cells: (usize, serde::de::IgnoredAny),
}

/// Total declared width of one grid's allocated rows.
struct DeclaredRowCells(usize);

impl<'de> Deserialize<'de> for DeclaredRowCells {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RowsVisitor;

        impl<'de> Visitor<'de> for RowsVisitor {
            type Value = DeclaredRowCells;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "at most {MAX_CHECKPOINT_GRID_ROWS} grid rows")
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut rows = 0_usize;
                let mut cells = 0_usize;
                while let Some(row) = sequence.next_element::<Option<RowShape>>()? {
                    rows += 1;
                    if rows > MAX_CHECKPOINT_GRID_ROWS {
                        return Err(serde::de::Error::custom(
                            "grid rows exceed checkpoint bound",
                        ));
                    }
                    if let Some(row) = row {
                        cells = cells
                            .checked_add(row.cells.0)
                            .filter(|cells| *cells <= MAX_CHECKPOINT_CELLS)
                            .ok_or_else(cell_budget_error)?;
                    }
                }
                Ok(DeclaredRowCells(cells))
            }
        }

        deserializer.deserialize_seq(RowsVisitor)
    }
}

fn deserialize_tab_stops<'de, D>(deserializer: D) -> Result<Vec<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_sequence(
        deserializer,
        crate::row::MAX_CHECKPOINT_COLUMNS,
        "terminal tab stops",
    )
}

fn deserialize_sixel_palette<'de, D>(deserializer: D) -> Result<Vec<u32>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_sequence(deserializer, MAX_SIXEL_COLORS, "Sixel palette entries")
}

fn deserialize_palette<'de, D>(deserializer: D) -> Result<Vec<u32>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_sequence(deserializer, 256, "terminal palette entries")
}

fn deserialize_events<'de, D>(deserializer: D) -> Result<Vec<CheckpointEvent>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_sequence(deserializer, MAX_CHECKPOINT_EVENTS, "terminal events")
}

fn deserialize_event_bytes<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_sequence(
        deserializer,
        MAX_CHECKPOINT_EVENT_BODY_BYTES,
        "terminal event bytes",
    )
}

fn deserialize_bounded_sequence<'de, D, T>(
    deserializer: D,
    maximum: usize,
    name: &'static str,
) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct BoundedVisitor<T> {
        maximum: usize,
        name: &'static str,
        marker: PhantomData<T>,
    }

    impl<'de, T> Visitor<'de> for BoundedVisitor<T>
    where
        T: Deserialize<'de>,
    {
        type Value = Vec<T>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "at most {} {}", self.maximum, self.name)
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            if sequence
                .size_hint()
                .is_some_and(|length| length > self.maximum)
            {
                return Err(serde::de::Error::custom(format_args!(
                    "{} exceed checkpoint bound",
                    self.name
                )));
            }
            let mut values =
                Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(self.maximum));
            while let Some(value) = sequence.next_element()? {
                if values.len() == self.maximum {
                    return Err(serde::de::Error::custom(format_args!(
                        "{} exceed checkpoint bound",
                        self.name
                    )));
                }
                values.push(value);
            }
            Ok(values)
        }
    }

    deserializer.deserialize_seq(BoundedVisitor {
        maximum,
        name,
        marker: PhantomData,
    })
}

#[derive(Default)]
struct BoundedWriter {
    bytes: Vec<u8>,
    exceeded: bool,
}

impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(next) = self.bytes.len().checked_add(bytes.len()) else {
            self.exceeded = true;
            return Err(io::Error::other("terminal checkpoint byte count overflow"));
        };
        if next > MAX_TERMINAL_CHECKPOINT_BYTES {
            self.exceeded = true;
            return Err(io::Error::other("terminal checkpoint exceeds hard bound"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
    use serde_json::{Value, json};

    use super::*;

    fn config() -> TerminalConfig {
        TerminalConfig::default()
    }

    fn mutate_body(mut mutate: impl FnMut(&mut Value)) -> Vec<u8> {
        let terminal = Terminal::new(4, 2, config());
        let bytes = terminal.encode_checkpoint().unwrap();
        let mut value: Value = serde_json::from_slice(&bytes).unwrap();
        mutate(&mut value["body"]["terminal"]);
        let mut envelope: CheckpointEnvelopeV2 = serde_json::from_value(value).unwrap();
        envelope.body.image_sha256 =
            Sha256::digest(encode_json(&envelope.body.images).unwrap()).into();
        let body_bytes = encode_json(&envelope.body).unwrap();
        envelope.body_sha256 = Sha256::digest(&body_bytes).into();
        encode_json(&envelope).unwrap()
    }

    fn mutate_image(mut mutate: impl FnMut(&mut Value)) -> Vec<u8> {
        let mut terminal = Terminal::new(4, 2, config());
        terminal.set_cell_pixel_size(1, 1);
        terminal.advance(b"\x1b_Ga=t,f=32,s=1,v=1,i=7;/wAA/w==\x1b\\");
        terminal.advance(b"\x1b_Ga=p,i=7,p=9,C=1\x1b\\");
        let bytes = terminal.encode_checkpoint().unwrap();
        let mut value: Value = serde_json::from_slice(&bytes).unwrap();
        mutate(&mut value["body"]["images"]);
        let mut envelope: CheckpointEnvelopeV2 = serde_json::from_value(value).unwrap();
        envelope.body.image_sha256 =
            Sha256::digest(encode_json(&envelope.body.images).unwrap()).into();
        envelope.body_sha256 = Sha256::digest(encode_json(&envelope.body).unwrap()).into();
        encode_json(&envelope).unwrap()
    }

    #[test]
    fn declared_cells_sum_both_grids_against_the_aggregate_budget() {
        let row = |columns: usize| json!({"cells": [columns, "x"], "dirty": false});
        let full = MAX_CHECKPOINT_CELLS / crate::row::MAX_CHECKPOINT_COLUMNS;
        let wide = || vec![row(crate::row::MAX_CHECKPOINT_COLUMNS); full];
        // Schema v1 keeps the grids in the body; v2 nests them under `terminal`.
        for nested in [false, true] {
            let header = |normal: Vec<Value>, alternate: Vec<Value>| {
                let grids = json!({"normal": {"rows": normal}, "alternate": {"rows": alternate}});
                let body = if nested {
                    json!({"terminal": grids, "images": {}})
                } else {
                    grids
                };
                serde_json::to_vec(&json!({"magic": vec![0_u8; 8], "schema": 1, "body": body}))
                    .unwrap()
            };
            decode_header(&header(wide(), Vec::new())).unwrap();
            decode_header(&header(Vec::new(), wide())).unwrap();
            let mut with_null = wide();
            with_null.push(Value::Null);
            decode_header(&header(with_null, Vec::new())).unwrap();
            // One more cell in either grid crosses the shared budget.
            for (normal, alternate) in [
                (wide(), vec![row(1)]),
                ([wide(), vec![row(1)]].concat(), Vec::new()),
                (vec![row(usize::MAX)], vec![row(1)]),
            ] {
                assert!(matches!(
                    decode_header(&header(normal, alternate)),
                    Err(CheckpointError::Encoding(_))
                ));
            }
        }
    }

    #[test]
    fn digest_consistent_but_invalid_internal_states_fail_without_panicking() {
        let corruptions = [
            mutate_body(|body| body["normal"]["row_ids"][0] = json!(0)),
            mutate_body(|body| body["normal"]["next_row_id"] = json!(1)),
            mutate_body(|body| {
                let row = body["normal"]["rows"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|row| !row.is_null())
                    .unwrap();
                let mut packed = Vec::new();
                for content in [0, u32::from(b'A'), u32::MAX] {
                    let mut cell = [0_u8; crate::cell::CHECKPOINT_CELL_BYTES];
                    cell[..4].copy_from_slice(&content.to_le_bytes());
                    packed.extend_from_slice(&cell);
                }
                row["cells"] = json!([4, BASE64_STANDARD.encode(packed)]);
            }),
            mutate_body(|body| body["normal"]["generation"] = json!(u64::MAX)),
            mutate_body(|body| body["normal"]["history_generation"] = json!(u64::MAX)),
            mutate_body(|body| body["normal"]["next_row_id"] = json!(u64::MAX)),
            mutate_body(|body| body["revision"] = json!(u64::MAX)),
            mutate_body(|body| body["parser"]["osc_limit"] = json!(1)),
            mutate_body(|body| body["tab_stops"] = json!([])),
            mutate_body(|body| body["scroll_region"]["end"] = json!(99)),
            mutate_body(|body| body["sixel_shared_palette"] = json!([])),
            mutate_body(|body| body["palette"] = json!([])),
            mutate_body(|body| {
                body["title"] = json!("x".repeat(MAX_CHECKPOINT_TITLE_BYTES + 1));
            }),
        ];
        for bytes in corruptions {
            assert!(matches!(
                Terminal::restore_checkpoint(&bytes, config()),
                Err(CheckpointError::InvalidState)
            ));
        }
    }

    #[test]
    fn digest_consistent_image_corruption_fails_without_partial_restore() {
        let corruptions = [
            mutate_image(|images| {
                images["plane"]["contents"][0]["pixels"] = json!("AAAAAA==");
            }),
            mutate_image(|images| {
                let byte = images["plane"]["contents"][0]["metadata"]["digest"][0]
                    .as_u64()
                    .unwrap();
                images["plane"]["contents"][0]["metadata"]["digest"][0] = json!(byte ^ 1);
            }),
            mutate_image(|images| {
                let duplicate = images["plane"]["contents"][0].clone();
                images["plane"]["contents"]
                    .as_array_mut()
                    .unwrap()
                    .push(duplicate);
            }),
            mutate_image(|images| images["plane"]["next_content_id"] = json!(1)),
            mutate_image(|images| {
                images["plane"]["placements"][0]["row_id"] = json!(u64::MAX);
            }),
            mutate_image(|images| {
                images["kitty_image_ids"]["normal"][0]["content_id"] = json!(u64::MAX);
            }),
            mutate_image(|images| {
                images["plane"]["contents"][0]["metadata"]["retention"] = json!("while_placed");
            }),
            // Unplaced WhilePlaced content cannot exist live; it is reclaimed at once.
            mutate_image(|images| {
                images["plane"]["contents"][0]["metadata"]["retention"] = json!("while_placed");
                images["plane"]["placements"] = json!([]);
                images["kitty_image_ids"]["normal"] = json!([]);
                images["kitty_image_ids"]["alternate"] = json!([]);
            }),
            mutate_image(|images| {
                let mut out_of_order = images["kitty_image_ids"]["normal"][0].clone();
                out_of_order["application_id"] = json!(6);
                images["kitty_image_ids"]["normal"]
                    .as_array_mut()
                    .unwrap()
                    .push(out_of_order);
            }),
        ];
        for bytes in corruptions {
            assert!(matches!(
                Terminal::restore_checkpoint(&bytes, config()),
                Err(CheckpointError::InvalidState)
            ));
        }
    }

    #[test]
    fn restored_kitty_ids_reject_aliases_of_one_content() {
        // Ids still strictly increase, so only content uniqueness refuses it.
        let alias = mutate_image(|images| {
            let mut alias = images["kitty_image_ids"]["normal"][0].clone();
            alias["application_id"] = json!(8);
            images["kitty_image_ids"]["normal"]
                .as_array_mut()
                .unwrap()
                .push(alias);
        });
        assert!(matches!(
            Terminal::restore_checkpoint(&alias, config()),
            Err(CheckpointError::InvalidState)
        ));
    }

    fn kitty_terminal() -> (Terminal, ImageContentId) {
        let mut terminal = Terminal::new(4, 2, config());
        terminal.set_cell_pixel_size(1, 1);
        terminal.advance(b"\x1b_Ga=t,f=32,s=1,v=1,i=7;/wAA/w==\x1b\\");
        terminal.advance(b"\x1b_Ga=p,i=7,p=9,C=1\x1b\\");
        let content = terminal
            .kitty_image_ids
            .get(ActiveScreen::Normal, 7)
            .unwrap();
        (terminal, content)
    }

    #[test]
    fn removing_image_content_drops_its_kitty_ids_and_still_checkpoints() {
        let (mut terminal, content) = kitty_terminal();
        terminal.remove_image_content(content).unwrap();
        assert!(terminal.kitty_image_ids.normal.is_empty());
        let bytes = terminal.encode_checkpoint().unwrap();
        let mut restored = Terminal::restore_checkpoint(&bytes, config()).unwrap();
        assert_eq!(restored.encode_checkpoint().unwrap(), bytes);
        // The removed id names nothing, so placing it again fails.
        restored.drain_events().for_each(drop);
        restored.advance(b"\x1b_Ga=p,i=7,p=9,C=1\x1b\\");
        assert!(
            restored
                .images
                .placements(ActiveScreen::Normal)
                .next()
                .is_none()
        );
    }

    #[test]
    fn capture_refuses_stale_or_aliased_kitty_ids() {
        let (mut stale, content) = kitty_terminal();
        stale
            .images
            .remove_content(ActiveScreen::Normal, content)
            .unwrap();
        assert!(matches!(
            stale.encode_checkpoint(),
            Err(CheckpointError::InvalidState)
        ));

        let (mut aliased, content) = kitty_terminal();
        aliased
            .kitty_image_ids
            .insert(ActiveScreen::Normal, 8, content);
        assert!(matches!(
            aliased.encode_checkpoint(),
            Err(CheckpointError::InvalidState)
        ));
    }

    #[test]
    fn wrong_magic_schema_and_body_digest_fail_closed() {
        for mutate in [
            (|envelope: &mut CheckpointEnvelopeV2| envelope.magic[0] ^= 1)
                as fn(&mut CheckpointEnvelopeV2),
            |envelope| envelope.schema += 1,
            |envelope| envelope.body_sha256[0] ^= 1,
        ] {
            let terminal = Terminal::new(4, 2, config());
            let bytes = terminal.encode_checkpoint().unwrap();
            let mut envelope: CheckpointEnvelopeV2 = serde_json::from_slice(&bytes).unwrap();
            mutate(&mut envelope);
            let corrupted = encode_json(&envelope).unwrap();
            assert!(matches!(
                Terminal::restore_checkpoint(&corrupted, config()),
                Err(CheckpointError::InvalidState)
            ));
        }
    }

    #[test]
    fn schema_v1_text_checkpoints_remain_restorable() {
        let terminal = Terminal::new(4, 2, config());
        let body = TerminalCheckpointV1::capture(&terminal).unwrap();
        let body_sha256 = Sha256::digest(encode_json(&body).unwrap()).into();
        let envelope = CheckpointEnvelopeV1 {
            magic: CHECKPOINT_MAGIC_V1,
            schema: TERMINAL_CHECKPOINT_SCHEMA_V1,
            body_sha256,
            body,
        };
        let encoded = encode_json(&envelope).unwrap();
        assert_eq!(
            Terminal::validate_checkpoint_encoding(&encoded).unwrap(),
            TERMINAL_CHECKPOINT_SCHEMA_V1
        );
        let restored = Terminal::restore_checkpoint(&encoded, config()).unwrap();
        assert_eq!(restored.normal, terminal.normal);
        assert_eq!(restored.alternate, terminal.alternate);
        assert_eq!(restored.images.metrics(), crate::ImageMetrics::default());
    }
}
