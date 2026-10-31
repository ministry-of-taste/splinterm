//! Foot-derived terminal coordinator and command semantics.
//!
//! Printing and controls follow Foot 1.27.0 `terminal.c`, `commands.c`,
//! `csi.c`, and `osc.c` at commit
//! `3c5b584b0eafa772eb4376fb6eaf6643399e190e`.

mod checkpoint;

pub use checkpoint::{
    CheckpointError, MAX_IMAGE_STATE_CHECKPOINT_BYTES, MAX_TERMINAL_CHECKPOINT_BYTES,
    MAX_TERMINAL_STATE_CHECKPOINT_BYTES, TERMINAL_CHECKPOINT_SCHEMA_V1,
    TERMINAL_CHECKPOINT_SCHEMA_V2,
};

use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    time::{Duration, Instant},
};

use unicode_width::UnicodeWidthChar;

use crate::{
    ActiveScreen, Attributes, CellContent, ChangeSet, ClipboardSelection, Color, ColorSource,
    ComposedTable, Coordinate, Cursor, CursorSnapshot, Dimensions, Grid, ImageAlphaMode,
    ImageContent, ImageContentId, ImageErasePolicy, ImageError, ImageMetrics, ImagePlacement,
    ImagePlacementId, ImagePlane, ImageRetention, ImageSourceFormat, MouseTracking,
    NewImageContent, NewImagePlacement, NewImagePlacementOptions, OSC52_MAX_BYTES, PixelRect,
    PixelSize, ReplyTerminator, ResnapshotRequired, RowSnapshot, ScrollDirection, ScrollRegion,
    ScrollbackSnapshot, SearchMatch, SearchPage, SharedKittyUploadBudget, SnapshotRequest,
    TerminalConfig, TerminalDamage, TerminalEvent, TerminalModes, TerminalRevision,
    TerminalSnapshot, TerminalUpdate, UnderlineStyle, UpdateBatch,
    image::{
        KittyUploadReservation, MAX_SIXEL_COLORS, SixelDecoder, SixelError, SixelImage, iterm,
        kitty::{self, Command as KittyCommand, Error as KittyError},
    },
    vt::{Action, Param, Params, Parser, StringTerminator},
};

/// Renderer-independent terminal state and streaming VT parser.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent parser and protocol modes are explicit terminal state"
)]
pub struct Terminal {
    normal: Grid,
    alternate: Grid,
    active: ActiveScreen,
    parser: Parser,
    config: TerminalConfig,
    attributes: Attributes,
    saved_attributes: Attributes,
    scroll_region: ScrollRegion,
    modes: TerminalModes,
    synchronized_updates: bool,
    synchronized_change: ChangeSet,
    tab_stops: Vec<bool>,
    composed: ComposedTable,
    images: ImagePlane,
    sixel_decoder: Option<Box<SixelDecoder>>,
    kitty_chunk: Option<KittyChunk>,
    kitty_upload: Option<KittyUpload>,
    iterm_transfer: Option<ItermTransfer>,
    kitty_upload_budget: SharedKittyUploadBudget,
    kitty_image_ids: KittyImageIds,
    cell_pixels: Option<(u32, u32)>,
    sixel_scrolling: bool,
    sixel_cursor_right: bool,
    sixel_palette_mode: SixelPaletteMode,
    sixel_shared_palette: Box<[u32; MAX_SIXEL_COLORS]>,
    sixel_palette_size: usize,
    sixel_maximum_width: u32,
    sixel_maximum_height: u32,
    title: String,
    palette: [u32; 256],
    initial_palette: [u32; 256],
    default_colors: [u32; 3],
    initial_default_colors: [u32; 3],
    events: Vec<TerminalEvent>,
    event_overflowed: bool,
    revision: TerminalRevision,
    update_history: VecDeque<TerminalUpdate>,
    current_change: Option<ChangeSet>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ItermTransfer {
    metadata: Vec<u8>,
    metadata_truncated: bool,
    encoded: Vec<u8>,
    payload_too_big: bool,
    payload_no_space: bool,
    reservation: Option<KittyUploadReservation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct KittyChunk {
    control: Vec<u8>,
    control_truncated: bool,
    encoded: Vec<u8>,
    encoded_limit: usize,
    payload_too_big: bool,
    payload_no_space: bool,
    reservation: Option<KittyUploadReservation>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct KittyCorrelation {
    image_id: u32,
    placement_id: u32,
    quiet: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct KittyUpload {
    command: KittyCommand,
    decoded_chunks: Vec<u8>,
    encoded_bytes: usize,
    reservations: Vec<KittyUploadReservation>,
}

impl Drop for KittyUpload {
    fn drop(&mut self) {
        self.reservations.clear();
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct KittyImageIds {
    normal: BTreeMap<u32, ImageContentId>,
    alternate: BTreeMap<u32, ImageContentId>,
}

impl KittyImageIds {
    fn get(&self, screen: ActiveScreen, id: u32) -> Option<ImageContentId> {
        self.map(screen).get(&id).copied()
    }

    fn insert(&mut self, screen: ActiveScreen, id: u32, content: ImageContentId) {
        self.map_mut(screen).insert(id, content);
    }

    fn remove(&mut self, screen: ActiveScreen, id: u32) {
        self.map_mut(screen).remove(&id);
    }

    /// Drops every application id naming removed content.
    fn remove_content(&mut self, screen: ActiveScreen, content: ImageContentId) {
        self.map_mut(screen).retain(|_, mapped| *mapped != content);
    }

    fn clear(&mut self, screen: ActiveScreen) {
        self.map_mut(screen).clear();
    }

    fn map(&self, screen: ActiveScreen) -> &BTreeMap<u32, ImageContentId> {
        match screen {
            ActiveScreen::Normal => &self.normal,
            ActiveScreen::Alternate => &self.alternate,
        }
    }

    fn map_mut(&mut self, screen: ActiveScreen) -> &mut BTreeMap<u32, ImageContentId> {
        match screen {
            ActiveScreen::Normal => &mut self.normal,
            ActiveScreen::Alternate => &mut self.alternate,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ActionBaseline {
    active: ActiveScreen,
    normal_generation: u64,
    alternate_generation: u64,
    cursor: Cursor,
    offset: usize,
    view: usize,
    modes: TerminalModes,
    scroll_region: ScrollRegion,
    title: String,
    palette: [u32; 256],
    default_colors: [u32; 3],
    image_metrics: ImageMetrics,
    row_before: Option<(i32, crate::Row)>,
    visible_before: Option<Vec<crate::Row>>,
    scrollback_rows: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum SixelPaletteMode {
    Private,
    Shared,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActionHint {
    Print,
    Execute(u8),
    Esc(u8),
    Csi {
        private: Option<u8>,
        final_byte: u8,
        first_param: u32,
    },
    Osc,
    Dcs,
    Truncated,
}

impl ActionHint {
    fn from_action(action: &Action) -> Self {
        match action {
            Action::Print(_) => Self::Print,
            Action::Execute(byte) => Self::Execute(*byte),
            Action::Esc { final_byte, .. } => Self::Esc(*final_byte),
            Action::Csi {
                private,
                final_byte,
                params,
                ..
            } => Self::Csi {
                private: *private,
                final_byte: *final_byte,
                first_param: params.get(0).value(0, false),
            },
            Action::Osc(..) => Self::Osc,
            Action::Dcs(..)
            | Action::KittyBegin(..)
            | Action::KittyCommand(..)
            | Action::KittyData(..)
            | Action::KittyEnd
            | Action::KittyAbort
            | Action::ItermBegin(..)
            | Action::ItermData(..)
            | Action::ItermEnd
            | Action::ItermAbort
            | Action::SixelBegin(..)
            | Action::SixelData(..)
            | Action::SixelEnd
            | Action::SixelAbort => Self::Dcs,
            Action::StringTruncated(..) => Self::Truncated,
        }
    }
}

impl Terminal {
    /// Constructs a terminal with normal and alternate screen buffers.
    ///
    /// # Panics
    ///
    /// Panics if either dimension is zero or outside Foot's signed coordinate
    /// space, or if configured grid capacity overflows Foot's limit.
    #[must_use]
    pub fn new(columns: usize, rows: usize, config: TerminalConfig) -> Self {
        assert!(
            columns > 0 && rows > 0,
            "terminal dimensions must be non-zero"
        );
        let requested = config.scrollback_lines.saturating_add(rows).max(rows);
        let normal_capacity = requested
            .checked_next_power_of_two()
            .expect("normal grid capacity overflow")
            .min(1_usize << 30);
        assert!(normal_capacity >= rows, "normal grid capacity is too small");
        let alternate_capacity = rows
            .checked_next_power_of_two()
            .expect("alternate grid capacity overflow")
            .min(1_usize << 30);
        assert!(
            alternate_capacity >= rows,
            "alternate grid capacity is too small"
        );
        assert!(config.tab_width > 0, "tab width must be non-zero");
        assert!(config.event_limit > 0, "event limit must be non-zero");
        assert!(
            config.update_history_limit > 0,
            "update history limit must be non-zero"
        );

        let palette = default_palette();
        let default_colors = [0x00ff_ffff, 0x0000_0000, 0x00ff_ffff];
        let sixel_shared_palette = sixel_palette(&config.sixel.palette);
        let mut terminal = Self {
            normal: Grid::with_screen_size(normal_capacity, columns, rows),
            alternate: Grid::with_screen_size(alternate_capacity, columns, rows),
            active: ActiveScreen::Normal,
            parser: Parser::new(config.osc_limit, config.dcs_limit),
            attributes: Attributes::default(),
            saved_attributes: Attributes::default(),
            scroll_region: ScrollRegion::new(0, i32::try_from(rows).expect("rows fit in i32")),
            modes: TerminalModes::default(),
            synchronized_updates: false,
            synchronized_change: ChangeSet::default(),
            tab_stops: Vec::new(),
            composed: ComposedTable::new(config.composed_limit),
            images: ImagePlane::new_with_shared_budget(
                config.image_limits,
                config.shared_image_budget.clone(),
            ),
            sixel_decoder: None,
            kitty_chunk: None,
            kitty_upload: None,
            iterm_transfer: None,
            kitty_upload_budget: config
                .shared_kitty_upload_budget
                .clone()
                .unwrap_or_else(|| {
                    SharedKittyUploadBudget::new(
                        crate::image::DEFAULT_KITTY_UPLOAD_BYTES_PER_DAEMON,
                    )
                }),
            kitty_image_ids: KittyImageIds::default(),
            cell_pixels: None,
            sixel_scrolling: true,
            sixel_cursor_right: false,
            sixel_palette_mode: if config.sixel.private_palette {
                SixelPaletteMode::Private
            } else {
                SixelPaletteMode::Shared
            },
            sixel_shared_palette,
            sixel_palette_size: MAX_SIXEL_COLORS,
            sixel_maximum_width: config.image_limits.maximum_dimension,
            sixel_maximum_height: config.image_limits.maximum_dimension,
            title: String::new(),
            palette,
            initial_palette: palette,
            default_colors,
            initial_default_colors: default_colors,
            events: Vec::new(),
            event_overflowed: false,
            revision: TerminalRevision::default(),
            update_history: VecDeque::new(),
            current_change: None,
            config,
        };
        terminal.reset_tab_stops(columns);
        terminal
    }

    /// Feeds arbitrary bytes into the persistent parser state.
    pub fn advance(&mut self, bytes: &[u8]) {
        self.advance_inner(bytes, false);
    }

    /// Feeds bytes through the next synchronized-frame completion boundary.
    ///
    /// Returns the number of consumed bytes. A caller can capture immutable
    /// state at that boundary before feeding the returned suffix.
    pub fn advance_to_synchronized_boundary(&mut self, bytes: &[u8]) -> (usize, bool) {
        self.advance_inner(bytes, true)
    }

    fn advance_inner(
        &mut self,
        bytes: &[u8],
        stop_at_synchronized_boundary: bool,
    ) -> (usize, bool) {
        for (offset, &byte) in bytes.iter().enumerate() {
            let synchronized_before = self.synchronized_updates;
            let mut reprocess = true;
            while reprocess {
                let (action, again) = self.parser.feed(byte);
                if let Some(action) = action {
                    match action {
                        Action::SixelBegin(params) => self.begin_sixel(&params),
                        Action::SixelData(byte) => self.put_sixel(byte),
                        Action::SixelEnd => self.end_sixel(),
                        Action::SixelAbort => self.abort_sixel(),
                        Action::KittyBegin(control, truncated) => {
                            self.begin_kitty(control, truncated);
                        }
                        Action::KittyCommand(control, truncated) => {
                            self.command_kitty(control, truncated);
                        }
                        Action::KittyData(byte) => self.put_kitty(byte),
                        Action::KittyEnd => self.end_kitty(),
                        Action::KittyAbort => self.abort_kitty(),
                        Action::ItermBegin(metadata, truncated) => {
                            self.begin_iterm(metadata, truncated);
                        }
                        Action::ItermData(byte) => self.put_iterm(byte),
                        Action::ItermEnd => self.end_iterm(),
                        Action::ItermAbort => self.abort_iterm(),
                        action => self.dispatch(action),
                    }
                }
                reprocess = again;
            }
            if stop_at_synchronized_boundary && synchronized_before && !self.synchronized_updates {
                return (offset + 1, true);
            }
        }
        (bytes.len(), false)
    }

    /// Reflows the normal screen and resizes the alternate screen without
    /// scrollback reflow.
    ///
    /// # Panics
    ///
    /// Panics if dimensions are zero, exceed Foot's coordinate limits, or
    /// overflow the configured grid capacities.
    pub fn resize(&mut self, columns: usize, rows: usize) {
        if self.normal.columns() == columns && self.normal.screen_rows() == rows {
            return;
        }
        self.current_change = Some(ChangeSet::default());
        let normal_capacity = self
            .config
            .scrollback_lines
            .saturating_add(rows)
            .max(rows)
            .checked_next_power_of_two()
            .expect("normal grid capacity overflow")
            .min(1_usize << 30);
        let alternate_capacity = rows
            .checked_next_power_of_two()
            .expect("alternate grid capacity overflow")
            .min(1_usize << 30);
        let composed = &self.composed;
        let normal_image_anchors =
            self.normal
                .resize_with_reflow(normal_capacity, columns, rows, |key| composed.width(key));
        let alternate_image_anchors =
            self.alternate
                .resize_without_reflow(alternate_capacity, columns, rows);
        let image_metrics = self.images.metrics();
        let mut images_changed = false;
        if self.images.has_placements(ActiveScreen::Normal) {
            images_changed |= self
                .images
                .remap_anchors(ActiveScreen::Normal, &normal_image_anchors);
            self.images
                .retain_anchors(ActiveScreen::Normal, &self.normal.retained_row_ids());
            images_changed |= self.images.resolve_text_overlaps(
                ActiveScreen::Normal,
                &self.normal.ordered_retained_row_ids(),
            );
        }
        if self.images.has_placements(ActiveScreen::Alternate) {
            images_changed |= self
                .images
                .remap_anchors(ActiveScreen::Alternate, &alternate_image_anchors);
            self.images
                .retain_anchors(ActiveScreen::Alternate, &self.alternate.retained_row_ids());
            images_changed |= self
                .images
                .remove_text_placements_outside_columns(ActiveScreen::Alternate, columns);
            images_changed |= self.images.resolve_text_overlaps(
                ActiveScreen::Alternate,
                &self.alternate.ordered_retained_row_ids(),
            );
        }
        self.scroll_region = ScrollRegion::new(0, i32::try_from(rows).expect("rows fit in i32"));
        self.reset_tab_stops(columns);
        let change = self
            .current_change
            .as_mut()
            .expect("resize transaction active");
        change.full();
        change.push(TerminalDamage::Dimensions);
        change.push(TerminalDamage::Viewport);
        change.push(TerminalDamage::Scrollback);
        if images_changed || self.images.metrics() != image_metrics {
            change.push(TerminalDamage::Images {
                screen: self.active,
            });
        }
        self.commit_change();
    }

    /// Returns the active grid.
    #[must_use]
    pub fn grid(&self) -> &Grid {
        match self.active {
            ActiveScreen::Normal => &self.normal,
            ActiveScreen::Alternate => &self.alternate,
        }
    }

    /// Returns the selected screen buffer.
    #[must_use]
    pub const fn active_screen(&self) -> ActiveScreen {
        self.active
    }

    /// Returns core mode state.
    #[must_use]
    pub const fn modes(&self) -> TerminalModes {
        self.modes
    }

    /// Whether application synchronized-update publication is currently active.
    #[must_use]
    pub const fn synchronized_updates(&self) -> bool {
        self.synchronized_updates
    }

    /// Ends a synchronized-update episode and commits its bounded final state.
    pub fn expire_synchronized_updates(&mut self) {
        if !self.synchronized_updates {
            return;
        }
        self.synchronized_updates = false;
        self.current_change = Some(ChangeSet::default());
        self.commit_change();
    }

    /// Returns the current title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns the current 256-color palette.
    #[must_use]
    pub const fn palette(&self) -> &[u32; 256] {
        &self.palette
    }

    /// Returns default foreground, background, and cursor colors.
    #[must_use]
    pub const fn default_colors(&self) -> &[u32; 3] {
        &self.default_colors
    }

    #[must_use]
    pub const fn revision(&self) -> TerminalRevision {
        self.revision
    }

    /// Sets the pixel dimensions used to convert decoded image pixels into
    /// cell-relative placement extents. Zero dimensions mark geometry unknown.
    pub const fn set_cell_pixel_size(&mut self, width: u32, height: u32) {
        self.cell_pixels = if width == 0 || height == 0 {
            None
        } else {
            Some((width, height))
        };
    }

    /// Returns the cell pixel geometry used by image protocol state.
    #[must_use]
    pub const fn cell_pixel_size(&self) -> Option<(u32, u32)> {
        self.cell_pixels
    }

    /// Returns current image-plane accounting and high-water marks.
    #[must_use]
    pub const fn image_metrics(&self) -> ImageMetrics {
        self.images.metrics()
    }

    /// Returns immutable canonical pixels for one content identity.
    #[must_use]
    pub fn image_content(&self, id: ImageContentId) -> Option<&ImageContent> {
        self.images.content(self.active, id)
    }

    /// Returns the stable row identity at the active cursor.
    ///
    /// # Panics
    ///
    /// Panics only if the terminal invariant that every cursor row is allocated
    /// has been violated.
    #[must_use]
    pub fn cursor_row_id(&self) -> u64 {
        self.grid()
            .row_id(self.grid().cursor().position().row)
            .expect("the active cursor row is allocated")
    }

    /// Atomically inserts content and a cursor-anchored placement in one revision.
    ///
    /// # Errors
    ///
    /// Returns a deterministic image validation or admission error without
    /// committing partial content or advancing the revision.
    pub fn insert_image_at_cursor(
        &mut self,
        content: NewImageContent<'_>,
        placement: NewImagePlacementOptions,
    ) -> Result<(ImageContentId, ImagePlacementId), ImageError> {
        let row_id = self.cursor_row_id();
        let identities =
            self.images
                .insert_content_and_placement(self.active, content, row_id, placement)?;
        self.commit_image_change();
        Ok(identities)
    }

    /// Inserts bounded canonical content on the active screen.
    ///
    /// # Errors
    ///
    /// Returns a deterministic image admission error without changing revision.
    pub fn insert_image_content(
        &mut self,
        input: NewImageContent<'_>,
    ) -> Result<ImageContentId, ImageError> {
        let id = self.images.insert_content(self.active, input)?;
        self.commit_image_change();
        Ok(id)
    }

    /// Inserts a bounded placement on the active screen.
    ///
    /// # Errors
    ///
    /// Returns an image error for an invalid anchor, crop, content, or budget.
    pub fn insert_image_placement(
        &mut self,
        input: NewImagePlacement,
    ) -> Result<ImagePlacementId, ImageError> {
        if !self.grid().retained_row_ids().contains(&input.row_id) {
            return Err(ImageError::InvalidAnchor);
        }
        let id = self.images.insert_placement(self.active, input)?;
        self.commit_image_change();
        Ok(id)
    }

    /// Removes one placement from the active screen.
    ///
    /// # Errors
    ///
    /// Returns [`ImageError::UnknownPlacement`] for absence or double removal.
    pub fn remove_image_placement(
        &mut self,
        id: ImagePlacementId,
    ) -> Result<ImagePlacement, ImageError> {
        let placement = self.images.remove_placement(self.active, id)?;
        self.commit_image_change();
        Ok(placement)
    }

    /// Removes content, all of its placements, and any Kitty image id naming
    /// it from the active screen.
    ///
    /// # Errors
    ///
    /// Returns [`ImageError::UnknownContent`] for absence or double removal.
    pub fn remove_image_content(&mut self, id: ImageContentId) -> Result<(), ImageError> {
        self.images.remove_content(self.active, id)?;
        self.kitty_image_ids.remove_content(self.active, id);
        self.commit_image_change();
        Ok(())
    }

    /// Creates a borrowed semantic snapshot without consuming updates/events.
    #[must_use]
    pub fn snapshot(&self, request: SnapshotRequest) -> TerminalSnapshot<'_> {
        let grid = self.grid();
        let visible_rows = grid
            .snapshot_identified_view_rows()
            .into_iter()
            .map(|(id, row)| RowSnapshot::visible(id, row, &self.composed))
            .collect();
        let (scrollback_rows, available, omitted, oldest, newest) =
            if self.active == ActiveScreen::Normal {
                self.normal
                    .snapshot_scrollback_rows(request.max_scrollback_rows)
            } else {
                (Vec::new(), 0, 0, None, None)
            };
        let returned_rows = scrollback_rows.len();
        let (oldest, newest) = if returned_rows == 0 {
            (None, None)
        } else {
            (oldest, newest)
        };
        let scrollback_rows = scrollback_rows
            .into_iter()
            .map(|(id, row)| RowSnapshot::scrollback(id, row, &self.composed))
            .collect();
        TerminalSnapshot::new(
            self.revision,
            Dimensions {
                columns: grid.columns(),
                rows: grid.screen_rows(),
            },
            self.active,
            CursorSnapshot {
                cursor: grid.cursor(),
                viewport_position: grid.cursor_in_view(),
            },
            self.modes,
            self.scroll_region,
            grid.view_follows_offset(),
            &self.title,
            &self.palette,
            &self.default_colors,
            self.images.content_metadata(self.active).collect(),
            self.images.ordered_placements(self.active),
            visible_rows,
            scrollback_rows,
            ScrollbackSnapshot {
                history_generation: self.normal.history_generation(),
                oldest_available_row_id: oldest,
                newest_available_row_id: newest,
                available_rows: available,
                returned_rows,
                omitted_oldest_rows: omitted,
            },
        )
    }

    /// Returns a bounded history page immediately before `before_row_id`.
    #[must_use]
    pub fn scrollback_page(
        &self,
        before_row_id: u64,
        maximum_rows: usize,
    ) -> crate::ScrollbackPage<'_> {
        let (rows, has_older) = if self.active == ActiveScreen::Normal {
            self.normal
                .snapshot_scrollback_page(before_row_id, maximum_rows)
        } else {
            (Vec::new(), false)
        };
        crate::ScrollbackPage {
            history_generation: self.normal.history_generation(),
            terminal_revision: self.revision,
            rows: rows
                .into_iter()
                .map(|(id, row)| RowSnapshot::scrollback(id, row, &self.composed))
                .collect(),
            has_older,
        }
    }

    /// Searches retained normal-screen rows newest-first without copying configured history.
    #[must_use]
    pub fn search_normal(
        &self,
        query: &str,
        case_sensitive: bool,
        from: impl Into<crate::SearchFrom>,
        maximum_results: usize,
        deadline: Duration,
    ) -> SearchPage {
        let from = from.into();
        let (skip_rows, before_row_id) = match from {
            crate::SearchFrom::Offset(skip_rows) => (skip_rows, None),
            crate::SearchFrom::BeforeRow(row_id) => (0, Some(row_id)),
        };
        let needle = if case_sensitive {
            query.to_owned()
        } else {
            query.to_lowercase()
        };
        let started = Instant::now();
        let mut matches = Vec::with_capacity(maximum_results.min(64));
        let mut scanned_rows = 0_usize;
        let mut timed_out = false;
        let mut has_older = false;
        let mut row_search = RowSearch::new(needle, case_sensitive);
        let mut next_before_row_id = None;
        let mut revision_bound = skip_rows > 0;
        let mut rows = self
            .normal
            .rows_reverse_placed()
            .skip(skip_rows)
            .skip_while(|(row_id, _, in_history)| {
                before_row_id.is_some_and(|before| !in_history || *row_id >= before)
            })
            .peekable();
        while let Some((row_id, row, in_history)) = rows.next() {
            // Every page reads at least one row, so a continuation always
            // moves on, and the row the deadline stops at is read next time.
            if scanned_rows > 0 && started.elapsed() >= deadline {
                timed_out = true;
                has_older = true;
                next_before_row_id = row_id.checked_add(1).filter(|_| in_history);
                break;
            }
            scanned_rows = scanned_rows.saturating_add(1);
            if let Some((start_column, end_column, preview)) =
                row_search.find(row.cells(), &self.composed)
            {
                revision_bound |= !in_history;
                matches.push(SearchMatch {
                    row_id,
                    start_column,
                    end_column,
                    preview,
                });
            }
            if matches.len() == maximum_results {
                has_older = rows.peek().is_some();
                next_before_row_id = Some(row_id).filter(|_| in_history);
                break;
            }
        }
        let next_before_row_id = next_before_row_id.filter(|_| has_older);
        revision_bound |= has_older && next_before_row_id.is_none();
        SearchPage {
            history_generation: self.normal.history_generation(),
            terminal_revision: self.revision,
            matches,
            has_older,
            next_offset: has_older.then_some(skip_rows.saturating_add(scanned_rows)),
            next_before_row_id,
            revision_bound,
            timed_out,
        }
    }

    /// Returns the number of contiguous retained updates after `base` without
    /// materializing or cloning their contents.
    ///
    /// # Errors
    ///
    /// Returns [`ResnapshotRequired`] when `base` is in the future or older
    /// than the retained update-history window.
    pub fn update_count_since(&self, base: TerminalRevision) -> Result<usize, ResnapshotRequired> {
        self.validate_update_base(base)?;
        Ok(self
            .update_history
            .iter()
            .filter(|update| update.revision() > base)
            .count())
    }

    /// Returns contiguous retained updates after `base` or requires a snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`ResnapshotRequired`] when `base` is in the future or older
    /// than the retained update-history window.
    pub fn updates_since(&self, base: TerminalRevision) -> Result<UpdateBatch, ResnapshotRequired> {
        self.validate_update_base(base)?;
        let updates = self
            .update_history
            .iter()
            .filter(|update| update.revision() > base)
            .cloned()
            .collect();
        Ok(UpdateBatch::new(base, self.revision, updates))
    }

    fn validate_update_base(&self, base: TerminalRevision) -> Result<(), ResnapshotRequired> {
        let oldest_base = self.update_history.front().map_or(self.revision, |update| {
            TerminalRevision::new(update.revision().value() - 1)
        });
        if base > self.revision || base < oldest_base {
            return Err(ResnapshotRequired::new(base, oldest_base, self.revision));
        }
        Ok(())
    }

    /// Drains one-shot semantic effects in parser order.
    pub fn drain_events(&mut self) -> impl Iterator<Item = TerminalEvent> + '_ {
        self.event_overflowed = false;
        self.events.drain(..)
    }

    fn push_event(&mut self, event: TerminalEvent) {
        // Clipboard contents stay out of terminal updates, which reach every
        // subscriber and the update history; only the drain sees them.
        let clipboard = matches!(
            event,
            TerminalEvent::ClipboardSet { .. } | TerminalEvent::ClipboardQuery { .. }
        );
        if !clipboard && let Some(change) = &mut self.current_change {
            change.events.push(event.clone());
        }
        if self.events.len() < self.config.event_limit {
            self.events.push(event);
        } else if !self.event_overflowed {
            self.event_overflowed = true;
            if let Some(last) = self.events.last_mut() {
                *last = TerminalEvent::EventQueueOverflow;
            }
            if let Some(change) = &mut self.current_change {
                change.events.push(TerminalEvent::EventQueueOverflow);
            }
        }
    }

    fn grid_mut(&mut self) -> &mut Grid {
        match self.active {
            ActiveScreen::Normal => &mut self.normal,
            ActiveScreen::Alternate => &mut self.alternate,
        }
    }

    fn prune_active_image_anchors(&mut self) {
        if !self.images.has_placements(self.active) {
            return;
        }
        let row_ids = self.grid().retained_row_ids();
        self.images.retain_anchors(self.active, &row_ids);
    }

    fn overwrite_image_cells(
        &mut self,
        start_row: usize,
        end_row: usize,
        start_column: usize,
        end_column: usize,
    ) {
        if !self.images.has_placements(self.active) {
            return;
        }
        let row_ids = self.grid().screen_row_ids();
        if self.images.remove_text_overlaps(
            self.active,
            &row_ids,
            start_row,
            end_row,
            start_column,
            end_column,
        ) {
            self.current_change
                .as_mut()
                .expect("text overwrite has an active transaction")
                .push(TerminalDamage::Images {
                    screen: self.active,
                });
        }
    }

    fn scroll_grid(
        &mut self,
        direction: ScrollDirection,
        region: ScrollRegion,
        rows: usize,
        background: Color,
    ) {
        let track_images =
            self.active == ActiveScreen::Normal && self.images.has_placements(ActiveScreen::Normal);
        let before = track_images.then(|| self.grid().screen_row_ids());
        let result = self.grid_mut().scroll(direction, region, rows, background);
        if let Some(before) = before
            && direction == ScrollDirection::Forward
            && region.start() == 0
            && usize::try_from(region.end()).ok() == Some(before.len())
            && result.rows > 0
        {
            let history = self.normal.newest_scrollback_row_ids(result.rows);
            let replacements: BTreeMap<_, _> =
                before.into_iter().take(result.rows).zip(history).collect();
            if self
                .images
                .remap_anchors(ActiveScreen::Normal, &replacements)
            {
                self.current_change
                    .as_mut()
                    .expect("scroll action has an active transaction")
                    .push(TerminalDamage::Images {
                        screen: ActiveScreen::Normal,
                    });
            }
        }
    }

    fn begin_iterm(&mut self, metadata: Vec<u8>, metadata_truncated: bool) {
        if let Some(upload) = self.kitty_upload.take() {
            self.reply_kitty(
                &upload.command,
                Err(KittyError::Invalid("interleaved image upload")),
            );
        }
        self.kitty_chunk = None;
        self.iterm_transfer = Some(ItermTransfer {
            metadata,
            metadata_truncated,
            encoded: Vec::new(),
            payload_too_big: false,
            payload_no_space: false,
            reservation: None,
        });
    }

    fn put_iterm(&mut self, byte: u8) {
        let Some(transfer) = &mut self.iterm_transfer else {
            return;
        };
        if transfer.reservation.is_none() && !transfer.payload_no_space {
            match self.kitty_upload_budget.reserve(iterm::MAX_ENCODED_BYTES) {
                Ok(reservation) => transfer.reservation = Some(reservation),
                Err(_) => transfer.payload_no_space = true,
            }
        }
        if transfer.payload_no_space {
            return;
        }
        if transfer.encoded.len() < iterm::MAX_ENCODED_BYTES {
            transfer.encoded.push(byte);
        } else {
            transfer.payload_too_big = true;
        }
    }

    fn abort_iterm(&mut self) {
        self.iterm_transfer = None;
    }

    fn end_iterm(&mut self) {
        let Some(transfer) = self.iterm_transfer.take() else {
            return;
        };
        let baseline = self.action_baseline(ActionHint::Osc);
        let images_before = self.images.clone();
        self.current_change = Some(ChangeSet::default());
        let result = self.process_iterm(&transfer);
        if result.is_err() {
            self.images = images_before;
            self.push_event(TerminalEvent::ImageRejected("iTerm2 inline image"));
        }
        self.prune_active_image_anchors();
        self.record_action_changes(&baseline, ActionHint::Osc);
        self.commit_change();
    }

    fn process_iterm(&mut self, transfer: &ItermTransfer) -> Result<(), KittyError> {
        if transfer.payload_no_space {
            return Err(KittyError::NoSpace("encoded upload budget exhausted"));
        }
        if transfer.payload_too_big {
            return Err(KittyError::TooBig("encoded iTerm2 payload exceeds limit"));
        }
        let command = iterm::parse_metadata(&transfer.metadata, transfer.metadata_truncated)?;
        let image = iterm::decode_png_payload(&transfer.encoded, command.declared_size)?;
        self.validate_kitty_dimensions(image.width, image.height)?;
        let (cell_width, cell_height) = self
            .cell_pixels
            .ok_or(KittyError::Invalid("iTerm2 cell geometry is unavailable"))?;
        let (columns, rows) = iterm_destination_extent(
            command,
            image.width,
            image.height,
            cell_width,
            cell_height,
            self.grid().columns(),
            self.grid().screen_rows(),
        )?;
        let cursor = self.grid().cursor().position();
        let row = usize::try_from(cursor.row)
            .map_err(|_| KittyError::Invalid("iTerm2 cursor row is invalid"))?;
        let column = usize::try_from(cursor.column)
            .map_err(|_| KittyError::Invalid("iTerm2 cursor column is invalid"))?;
        let row_id = self
            .grid()
            .screen_row_ids()
            .get(row)
            .copied()
            .ok_or(KittyError::Invalid("iTerm2 cursor anchor is unavailable"))?;
        self.images
            .insert_content_and_placement(
                self.active,
                NewImageContent {
                    width: image.width,
                    height: image.height,
                    source_format: image.format,
                    alpha_mode: image.alpha_mode,
                    pixels: &image.pixels,
                    retention: ImageRetention::WhilePlaced,
                },
                row_id,
                NewImagePlacementOptions {
                    column,
                    source: PixelRect {
                        x: 0,
                        y: 0,
                        width: image.width,
                        height: image.height,
                    },
                    destination: crate::CellExtent { columns, rows },
                    source_cell_size: Some(PixelSize {
                        width: cell_width,
                        height: cell_height,
                    }),
                    x_offset: 0,
                    y_offset: 0,
                    z_index: 0,
                    application_image_id: None,
                    application_placement_id: None,
                    erase_policy: ImageErasePolicy::TextOverwrite,
                },
            )
            .map_err(map_image_error)?;
        if !command.do_not_move_cursor {
            self.move_cursor(
                -i32::try_from(column).unwrap_or(i32::MAX),
                i32::try_from(rows).unwrap_or(i32::MAX),
            );
        }
        Ok(())
    }

    fn begin_kitty(&mut self, control: Vec<u8>, control_truncated: bool) {
        let encoded_limit = if self.kitty_upload.is_some()
            || kitty::parse_control(&control, control_truncated).is_ok_and(|command| command.more)
        {
            kitty::MAX_ENCODED_CHUNK_BYTES
        } else {
            kitty::MAX_ENCODED_UPLOAD_BYTES
        };
        self.kitty_chunk = Some(KittyChunk {
            control,
            control_truncated,
            encoded: Vec::new(),
            encoded_limit,
            payload_too_big: false,
            payload_no_space: false,
            reservation: None,
        });
    }

    fn command_kitty(&mut self, control: Vec<u8>, control_truncated: bool) {
        self.begin_kitty(control, control_truncated);
        self.end_kitty();
    }

    fn put_kitty(&mut self, byte: u8) {
        let Some(chunk) = &mut self.kitty_chunk else {
            return;
        };
        if chunk.reservation.is_none() && !chunk.payload_no_space {
            match self.kitty_upload_budget.reserve(chunk.encoded_limit) {
                Ok(reservation) => chunk.reservation = Some(reservation),
                Err(_) => chunk.payload_no_space = true,
            }
        }
        if chunk.payload_no_space {
            return;
        }
        if chunk.encoded.len() < chunk.encoded_limit {
            chunk.encoded.push(byte);
        } else {
            chunk.payload_too_big = true;
        }
    }

    fn abort_kitty(&mut self) {
        self.kitty_chunk = None;
        self.kitty_upload = None;
    }

    fn reject_kitty_chunk(
        &mut self,
        control: &[u8],
        fallback: KittyCorrelation,
        error: KittyError,
    ) {
        if let Some(mut upload) = self.kitty_upload.take() {
            let continuation = kitty_correlation(control);
            if control
                .split(|byte| *byte == b',')
                .any(|field| field.starts_with(b"q="))
                && continuation.quiet <= 2
            {
                upload.command.quiet = continuation.quiet;
            }
            self.reply_kitty(&upload.command, Err(error));
        } else {
            self.reply_kitty_correlation(fallback, Err(error));
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "chunk finalization keeps upload admission, continuation, and reply ordering atomic"
    )]
    fn end_kitty(&mut self) {
        let Some(mut chunk) = self.kitty_chunk.take() else {
            return;
        };
        let correlation = kitty_correlation(&chunk.control);
        let command = match kitty::parse_control(&chunk.control, chunk.control_truncated) {
            Ok(command) => command,
            Err(error) => {
                self.reject_kitty_chunk(&chunk.control, correlation, error);
                return;
            }
        };
        if chunk.payload_no_space {
            self.reject_kitty_chunk(
                &chunk.control,
                correlation,
                KittyError::NoSpace("encoded upload budget exhausted"),
            );
            return;
        }
        if chunk.payload_too_big {
            self.reject_kitty_chunk(
                &chunk.control,
                correlation,
                KittyError::TooBig("encoded chunk exceeds limit"),
            );
            return;
        }
        if self.kitty_upload.is_some() && command.action == kitty::Action::Delete {
            self.kitty_upload = None;
            self.process_kitty(command, Vec::new());
            return;
        }
        if command.more && chunk.encoded.len() % 4 != 0 {
            self.reject_kitty_chunk(
                &chunk.control,
                correlation,
                KittyError::Invalid("non-final chunk is not base64 aligned"),
            );
            return;
        }
        let decoded_limit = if command.more || self.kitty_upload.is_some() {
            3072
        } else {
            kitty::MAX_ENCODED_UPLOAD_BYTES
        };
        let decoded = match kitty::decode_base64_payload(
            &chunk.encoded,
            chunk.encoded_limit,
            decoded_limit,
        ) {
            Ok(decoded) => decoded,
            Err(error) => {
                self.reject_kitty_chunk(&chunk.control, correlation, error);
                return;
            }
        };
        if let Some(mut upload) = self.kitty_upload.take() {
            if !kitty_continuation_control(&chunk.control) {
                self.reply_kitty(
                    &upload.command,
                    Err(KittyError::Invalid("interleaved Kitty upload")),
                );
                return;
            }
            if upload.command.more && chunk.encoded.len() % 4 != 0 && command.more {
                self.reply_kitty(
                    &upload.command,
                    Err(KittyError::BadMessage(
                        "non-final chunk is not base64 aligned",
                    )),
                );
                return;
            }
            let Some(encoded_bytes) = upload.encoded_bytes.checked_add(chunk.encoded.len()) else {
                self.reply_kitty(
                    &upload.command,
                    Err(KittyError::TooBig("encoded upload overflow")),
                );
                return;
            };
            if encoded_bytes > kitty::MAX_ENCODED_UPLOAD_BYTES {
                self.reply_kitty(
                    &upload.command,
                    Err(KittyError::TooBig("encoded upload exceeds limit")),
                );
                return;
            }
            upload.encoded_bytes = encoded_bytes;
            upload.decoded_chunks.extend_from_slice(&decoded);
            upload.reservations.extend(chunk.reservation.take());
            if chunk
                .control
                .split(|byte| *byte == b',')
                .any(|field| field.starts_with(b"q="))
            {
                upload.command.quiet = command.quiet;
            }
            if command.more {
                self.kitty_upload = Some(upload);
            } else {
                let decoded_chunks = std::mem::take(&mut upload.decoded_chunks);
                self.process_kitty(upload.command, decoded_chunks);
            }
            return;
        }
        if command.more {
            if chunk.encoded.len() % 4 != 0 {
                self.reply_kitty(
                    &command,
                    Err(KittyError::BadMessage(
                        "non-final chunk is not base64 aligned",
                    )),
                );
                return;
            }
            if !matches!(
                command.action,
                kitty::Action::Transmit | kitty::Action::TransmitAndDisplay | kitty::Action::Query
            ) {
                self.reply_kitty(
                    &command,
                    Err(KittyError::Invalid("continuation requires transmission")),
                );
                return;
            }
            self.kitty_upload = Some(KittyUpload {
                command,
                decoded_chunks: decoded,
                encoded_bytes: chunk.encoded.len(),
                reservations: chunk.reservation.into_iter().collect(),
            });
        } else {
            self.process_kitty(command, decoded);
        }
    }

    fn process_kitty(&mut self, command: KittyCommand, bytes: Vec<u8>) {
        let hint = ActionHint::Dcs;
        let baseline = self.action_baseline(hint);
        let images_before = self.images.clone();
        let image_ids_before = self.kitty_image_ids.clone();
        self.current_change = Some(ChangeSet::default());
        let result = match command.action {
            kitty::Action::Transmit | kitty::Action::TransmitAndDisplay | kitty::Action::Query => {
                self.transmit_kitty(&command, bytes)
            }
            kitty::Action::Display => self.place_kitty(&command),
            kitty::Action::Delete => self.delete_kitty(&command),
        };
        if result.is_err() {
            self.images = images_before;
            self.kitty_image_ids = image_ids_before;
        }
        if command.action != kitty::Action::Delete || result.is_err() {
            self.reply_kitty(&command, result);
        }
        self.prune_active_image_anchors();
        self.record_action_changes(&baseline, hint);
        self.commit_change();
    }

    fn transmit_kitty(&mut self, command: &KittyCommand, bytes: Vec<u8>) -> Result<(), KittyError> {
        if command.medium != kitty::Medium::Direct {
            return Err(KittyError::Unsupported("unsupported transmission medium"));
        }
        let image = kitty::decode_image(command, bytes)?;
        self.validate_kitty_dimensions(image.width, image.height)?;
        if command.action == kitty::Action::Query {
            return Ok(());
        }
        let content = NewImageContent {
            width: image.width,
            height: image.height,
            source_format: image.format,
            alpha_mode: image.alpha_mode,
            pixels: &image.pixels,
            retention: if command.image_id == 0 {
                ImageRetention::WhilePlaced
            } else {
                ImageRetention::ExplicitDelete
            },
        };
        let previous = (command.image_id != 0)
            .then(|| self.kitty_image_ids.get(self.active, command.image_id))
            .flatten();
        if command.action == kitty::Action::TransmitAndDisplay
            && let Some(previous) = previous
        {
            let (placement, _, _) = self.kitty_placement_input(
                command,
                previous,
                Some(command.image_id),
                image.width,
                image.height,
            )?;
            self.images
                .preflight_replacement_placement(self.active, previous, content, placement)
                .map_err(map_image_error)?;
        }
        let content_id = if let Some(previous) = previous {
            self.images
                .replace_content(self.active, previous, content)
                .map_err(map_image_error)?
        } else {
            self.images
                .insert_content(self.active, content)
                .map_err(map_image_error)?
        };
        if command.image_id != 0 {
            self.kitty_image_ids
                .insert(self.active, command.image_id, content_id);
        }
        if command.action == kitty::Action::TransmitAndDisplay {
            self.place_kitty_content(
                command,
                content_id,
                (command.image_id != 0).then_some(command.image_id),
            )?;
        }
        Ok(())
    }

    fn place_kitty(&mut self, command: &KittyCommand) -> Result<(), KittyError> {
        if command.image_id == 0 {
            return Err(KittyError::Invalid("image id is required for placement"));
        }
        let content_id = self
            .kitty_image_ids
            .get(self.active, command.image_id)
            .ok_or(KittyError::NotFound("image id does not exist"))?;
        self.place_kitty_content(command, content_id, Some(command.image_id))
    }

    fn place_kitty_content(
        &mut self,
        command: &KittyCommand,
        content_id: ImageContentId,
        application_image_id: Option<u32>,
    ) -> Result<(), KittyError> {
        let metadata = self
            .images
            .content(self.active, content_id)
            .ok_or(KittyError::NotFound("image content does not exist"))?
            .metadata();
        let (placement, columns, rows) = self.kitty_placement_input(
            command,
            content_id,
            application_image_id,
            metadata.width,
            metadata.height,
        )?;
        let existing = self
            .images
            .placements(self.active)
            .filter(|placement| {
                application_image_id.is_some()
                    && placement.application_image_id == application_image_id
                    && command.placement_id != 0
                    && placement.application_placement_id == Some(command.placement_id)
            })
            .map(|placement| placement.id)
            .collect::<Vec<_>>();
        let mut candidate = self.images.clone();
        for placement_id in existing {
            candidate
                .remove_placement(self.active, placement_id)
                .map_err(map_image_error)?;
        }
        candidate
            .insert_placement(self.active, placement)
            .map_err(map_image_error)?;
        self.images = candidate;
        if !command.no_cursor_move {
            self.move_cursor(
                i32::try_from(columns).unwrap_or(i32::MAX),
                i32::try_from(rows).unwrap_or(i32::MAX),
            );
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Kitty crop, aspect, offset, and anchor geometry share one checked constructor"
    )]
    fn kitty_placement_input(
        &self,
        command: &KittyCommand,
        content_id: ImageContentId,
        application_image_id: Option<u32>,
        content_width: u32,
        content_height: u32,
    ) -> Result<(NewImagePlacement, usize, usize), KittyError> {
        let source_x = command.source_x.min(content_width);
        let source_y = command.source_y.min(content_height);
        let source_right = command.source_width.map_or(content_width, |width| {
            command.source_x.saturating_add(width).min(content_width)
        });
        let source_bottom = command.source_height.map_or(content_height, |height| {
            command.source_y.saturating_add(height).min(content_height)
        });
        let source_width = source_right.saturating_sub(source_x);
        let source_height = source_bottom.saturating_sub(source_y);
        if source_width == 0 || source_height == 0 {
            return Err(KittyError::Invalid("source crop is empty"));
        }
        let (cell_width, cell_height) = self.cell_pixels.unwrap_or((1, 1));
        if u32::try_from(command.x_offset).unwrap_or(u32::MAX) >= cell_width
            || u32::try_from(command.y_offset).unwrap_or(u32::MAX) >= cell_height
        {
            return Err(KittyError::Invalid("cell offset exceeds cell size"));
        }
        let (columns, rows) = match (command.columns, command.rows) {
            (Some(columns), Some(rows)) => (columns, rows),
            (Some(columns), None) => (
                columns,
                kitty_aspect_extent(
                    source_height,
                    columns,
                    cell_width,
                    source_width,
                    cell_height,
                )?,
            ),
            (None, Some(rows)) => (
                kitty_aspect_extent(source_width, rows, cell_height, source_height, cell_width)?,
                rows,
            ),
            (None, None) => (
                usize::try_from(source_width.div_ceil(cell_width)).unwrap_or(usize::MAX),
                usize::try_from(source_height.div_ceil(cell_height)).unwrap_or(usize::MAX),
            ),
        };
        if columns == 0 || rows == 0 {
            return Err(KittyError::Invalid("destination extent must be non-zero"));
        }
        let cursor = self.grid().cursor().position();
        let row = usize::try_from(cursor.row)
            .map_err(|_| KittyError::Invalid("cursor row is invalid"))?;
        let column = usize::try_from(cursor.column)
            .map_err(|_| KittyError::Invalid("cursor column is invalid"))?;
        let row_id = self
            .grid()
            .screen_row_ids()
            .get(row)
            .copied()
            .ok_or(KittyError::Invalid("cursor anchor is unavailable"))?;
        Ok((
            NewImagePlacement {
                content_id,
                row_id,
                column,
                source: PixelRect {
                    x: source_x,
                    y: source_y,
                    width: source_width,
                    height: source_height,
                },
                destination: crate::CellExtent { columns, rows },
                source_cell_size: Some(PixelSize {
                    width: cell_width,
                    height: cell_height,
                }),
                x_offset: command.x_offset,
                y_offset: command.y_offset,
                z_index: command.z_index,
                application_image_id,
                application_placement_id: (application_image_id.is_some()
                    && command.placement_id != 0)
                    .then_some(command.placement_id),
                erase_policy: ImageErasePolicy::ExplicitDelete,
            },
            columns,
            rows,
        ))
    }

    fn delete_kitty(&mut self, command: &KittyCommand) -> Result<(), KittyError> {
        let selector = command.delete_selector.unwrap_or(b'a');
        let remove_content = selector.is_ascii_uppercase();
        let selector = selector.to_ascii_lowercase();
        if !matches!(selector, b'a' | b'i') {
            return Err(KittyError::Unsupported("unsupported delete selector"));
        }
        if selector == b'i' && command.image_id == 0 {
            return Err(KittyError::Invalid("image id is required for selector"));
        }
        let ordered_rows = self.grid().ordered_retained_row_ids();
        let visible_rows = self
            .grid()
            .screen_row_ids()
            .into_iter()
            .collect::<HashSet<_>>();
        let matching = self
            .images
            .placements(self.active)
            .filter(|placement| {
                (selector == b'a'
                    && placement_intersects_rows(placement, &ordered_rows, &visible_rows))
                    || (placement.application_image_id == Some(command.image_id)
                        && (command.placement_id == 0
                            || placement.application_placement_id == Some(command.placement_id)))
            })
            .map(|placement| (placement.id, placement.content_id))
            .collect::<Vec<_>>();
        for (placement_id, _) in &matching {
            self.images
                .remove_placement(self.active, *placement_id)
                .map_err(map_image_error)?;
        }
        if remove_content {
            let content_ids = if selector == b'a' {
                matching
                    .iter()
                    .filter_map(|(_, content_id)| {
                        self.images.content(self.active, *content_id).and_then(|_| {
                            self.kitty_image_ids.map(self.active).iter().find_map(
                                |(application_id, mapped)| {
                                    (*mapped == *content_id).then_some((*application_id, *mapped))
                                },
                            )
                        })
                    })
                    .collect::<Vec<_>>()
            } else {
                self.kitty_image_ids
                    .get(self.active, command.image_id)
                    .map(|content_id| vec![(command.image_id, content_id)])
                    .unwrap_or_default()
            };
            let mut content_ids = content_ids;
            content_ids.sort_unstable_by_key(|(_, content_id)| content_id.value());
            content_ids.dedup_by_key(|(_, content_id)| content_id.value());
            for (application_id, content_id) in content_ids {
                let still_placed = self
                    .images
                    .placements(self.active)
                    .any(|placement| placement.content_id == content_id);
                if !still_placed {
                    self.images
                        .remove_content(self.active, content_id)
                        .map_err(map_image_error)?;
                    self.kitty_image_ids.remove(self.active, application_id);
                }
            }
        }
        Ok(())
    }

    fn validate_kitty_dimensions(&self, width: u32, height: u32) -> Result<(), KittyError> {
        let limits = self.config.image_limits;
        let pixels = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .ok_or(KittyError::TooBig("image dimensions overflow"))?;
        if width == 0
            || height == 0
            || width > limits.maximum_dimension
            || height > limits.maximum_dimension
            || pixels > limits.maximum_pixels
        {
            return Err(KittyError::TooBig("image dimensions exceed limit"));
        }
        Ok(())
    }

    fn reply_kitty(&mut self, command: &KittyCommand, result: Result<(), KittyError>) {
        let correlation = KittyCorrelation {
            image_id: command.image_id,
            placement_id: command.placement_id,
            quiet: command.quiet,
        };
        self.reply_kitty_correlation(correlation, result);
    }

    fn reply_kitty_correlation(
        &mut self,
        correlation: KittyCorrelation,
        result: Result<(), KittyError>,
    ) {
        if correlation.image_id == 0
            || correlation.quiet == 2
            || (correlation.quiet == 1 && result.is_ok())
        {
            return;
        }
        let mut reply = format!("\x1b_Gi={}", correlation.image_id);
        if correlation.placement_id != 0 {
            use std::fmt::Write as _;
            let _ = write!(reply, ",p={}", correlation.placement_id);
        }
        reply.push(';');
        match result {
            Ok(()) => reply.push_str("OK"),
            Err(error) => {
                reply.push_str(error.code());
                reply.push(':');
                reply.push_str(error.message());
                self.push_event(TerminalEvent::ImageRejected("Kitty graphics command"));
            }
        }
        reply.push_str("\x1b\\");
        if reply.len() <= 512 {
            self.push_event(TerminalEvent::PtyWrite(reply.into_bytes()));
        }
    }

    fn begin_sixel(&mut self, params: &Params) {
        if !self.config.sixel.enabled {
            self.sixel_decoder = None;
            return;
        }
        let private_palette;
        let initial_palette = if self.sixel_palette_mode == SixelPaletteMode::Private {
            private_palette = sixel_palette(&self.config.sixel.palette);
            private_palette.as_slice()
        } else {
            self.sixel_shared_palette.as_slice()
        };
        self.sixel_decoder = Some(Box::new(SixelDecoder::new(
            params.get(0).value(0, true),
            params.get(1).value(0, false),
            self.config.image_limits,
            self.sixel_palette_size,
            self.sixel_maximum_width,
            self.sixel_maximum_height,
            initial_palette,
        )));
    }

    fn put_sixel(&mut self, byte: u8) {
        let failed = self
            .sixel_decoder
            .as_mut()
            .is_some_and(|decoder| decoder.put(byte).is_err());
        if failed {
            self.abort_sixel();
            self.push_event(TerminalEvent::ImageRejected("Sixel decode limit"));
        }
    }

    fn abort_sixel(&mut self) {
        let Some(decoder) = self.sixel_decoder.take() else {
            return;
        };
        if self.sixel_palette_mode == SixelPaletteMode::Shared {
            self.sixel_shared_palette
                .as_mut_slice()
                .copy_from_slice(decoder.palette());
        }
    }

    fn finish_sixel_decoder(&mut self, decoder: SixelDecoder) -> Option<SixelImage> {
        match decoder.finish() {
            Ok(image) => {
                if self.sixel_palette_mode == SixelPaletteMode::Shared {
                    self.sixel_shared_palette.clone_from(&image.palette);
                }
                Some(image)
            }
            Err(
                SixelError::InputLimit
                | SixelError::Dimensions
                | SixelError::ExpansionRatio
                | SixelError::PixelWrites,
            ) => {
                self.push_event(TerminalEvent::ImageRejected("Sixel decode limit"));
                None
            }
            Err(SixelError::Malformed) => {
                self.push_event(TerminalEvent::ImageRejected("malformed Sixel"));
                None
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "pinned Foot placement, scrolling, and cursor effects commit as one image transaction"
    )]
    fn end_sixel(&mut self) {
        let Some(decoder) = self.sixel_decoder.take() else {
            return;
        };
        let Some(image) = self.finish_sixel_decoder(*decoder) else {
            return;
        };
        let Some((cell_width, cell_height)) = self.cell_pixels else {
            self.push_event(TerminalEvent::ImageRejected("Sixel geometry unavailable"));
            return;
        };
        let Some(columns) = image
            .width
            .checked_add(cell_width - 1)
            .map(|value| value / cell_width)
        else {
            self.push_event(TerminalEvent::ImageRejected("Sixel placement overflow"));
            return;
        };
        let Some(rows) = image
            .height
            .checked_add(cell_height - 1)
            .map(|value| value / cell_height)
        else {
            self.push_event(TerminalEvent::ImageRejected("Sixel placement overflow"));
            return;
        };
        let destination_rows = usize::try_from(rows).unwrap_or(usize::MAX);
        if destination_rows > self.grid().row_capacity() {
            self.push_event(TerminalEvent::ImageRejected(
                "Sixel image exceeds row capacity",
            ));
            return;
        }
        let cursor = self.grid().cursor().position();
        let start_row = if self.sixel_scrolling {
            usize::try_from(cursor.row).unwrap_or(0)
        } else {
            0
        };
        let start_column = if self.sixel_scrolling {
            usize::try_from(cursor.column).unwrap_or(0)
        } else {
            0
        };
        let row_order = self.grid().screen_row_ids();
        let Some(row_id) = row_order.get(start_row).copied() else {
            self.push_event(TerminalEvent::ImageRejected("Sixel anchor unavailable"));
            return;
        };
        let content = NewImageContent {
            width: image.width,
            height: image.height,
            source_format: ImageSourceFormat::Sixel,
            alpha_mode: if image.opaque {
                ImageAlphaMode::Opaque
            } else {
                ImageAlphaMode::Premultiplied
            },
            pixels: &image.pixels,
            retention: ImageRetention::WhilePlaced,
        };
        let placement = NewImagePlacementOptions {
            column: start_column,
            source: PixelRect {
                x: 0,
                y: 0,
                width: image.width,
                height: image.height,
            },
            destination: crate::CellExtent {
                columns: usize::try_from(columns).unwrap_or(usize::MAX),
                rows: destination_rows,
            },
            source_cell_size: Some(PixelSize {
                width: cell_width,
                height: cell_height,
            }),
            x_offset: 0,
            y_offset: 0,
            z_index: -1,
            application_image_id: None,
            application_placement_id: None,
            erase_policy: ImageErasePolicy::TextOverwrite,
        };
        let baseline = self.action_baseline(ActionHint::Dcs);
        self.current_change = Some(ChangeSet::default());
        if self
            .images
            .insert_sixel_content_and_placement(self.active, content, &row_order, row_id, placement)
            .is_err()
        {
            self.current_change = None;
            self.push_event(TerminalEvent::ImageRejected("Sixel image admission"));
            return;
        }
        if self.sixel_scrolling {
            let cursor_rows = image
                .cursor_pixel_row
                .checked_add(cell_height - 1)
                .map_or(0, |value| value / cell_height);
            for _ in 1..cursor_rows {
                self.line_feed();
            }
            let mut cursor = self.grid().cursor();
            let column = if self.sixel_cursor_right {
                start_column
                    .saturating_add(usize::try_from(columns).unwrap_or(usize::MAX))
                    .min(self.grid().columns() - 1)
            } else {
                start_column
            };
            let mut position = cursor.position();
            position.column = i32::try_from(column).unwrap_or(i32::MAX);
            cursor.set_position(position);
            cursor.set_deferred_wrap(false);
            self.grid_mut().set_cursor(cursor);
        }
        self.prune_active_image_anchors();
        self.record_action_changes(&baseline, ActionHint::Dcs);
        self.commit_change();
    }

    fn dispatch(&mut self, action: Action) {
        let hint = ActionHint::from_action(&action);
        let baseline = self.action_baseline(hint);
        self.current_change = Some(ChangeSet::default());
        self.dispatch_inner(action);
        self.prune_active_image_anchors();
        self.record_action_changes(&baseline, hint);
        self.commit_change();
    }

    fn dispatch_inner(&mut self, action: Action) {
        match action {
            Action::Print(character) => self.print(character),
            Action::Execute(byte) => self.execute(byte),
            Action::Esc {
                intermediates,
                intermediate_count,
                final_byte,
            } => self.esc(&intermediates[..intermediate_count], final_byte),
            Action::Csi {
                private,
                intermediates,
                intermediate_count,
                params,
                final_byte,
            } => self.csi(
                private,
                &intermediates[..intermediate_count],
                &params,
                final_byte,
            ),
            Action::Osc(payload, terminator) => self.osc(&payload, terminator),
            Action::Dcs(_) => self.push_event(TerminalEvent::UnsupportedSequence("DCS")),
            Action::SixelBegin(_)
            | Action::SixelData(_)
            | Action::SixelEnd
            | Action::SixelAbort
            | Action::KittyBegin(..)
            | Action::KittyCommand(..)
            | Action::KittyData(..)
            | Action::KittyEnd
            | Action::KittyAbort
            | Action::ItermBegin(..)
            | Action::ItermData(..)
            | Action::ItermEnd
            | Action::ItermAbort => {
                unreachable!("streaming image actions bypass ordinary transactions")
            }
            Action::StringTruncated(kind) => {
                self.push_event(TerminalEvent::StringTruncated(kind));
            }
        }
    }

    fn action_baseline(&self, hint: ActionHint) -> ActionBaseline {
        let grid = self.grid();
        let row_before = match hint {
            ActionHint::Csi {
                final_byte: b'K' | b'@' | b'P' | b'X',
                ..
            } => grid
                .row(grid.cursor().position().row)
                .cloned()
                .map(|row| (grid.cursor().position().row, row)),
            _ => None,
        };
        let visible_before = match hint {
            ActionHint::Csi {
                final_byte: b'J',
                first_param,
                ..
            } if first_param != 3 => Some(grid.snapshot_view_rows().into_iter().cloned().collect()),
            _ => None,
        };
        let scrollback_rows = matches!(
            hint,
            ActionHint::Csi {
                final_byte: b'J',
                first_param: 3,
                ..
            }
        )
        .then(|| grid.snapshot_scrollback_rows(usize::MAX).1);
        ActionBaseline {
            active: self.active,
            normal_generation: self.normal.generation(),
            alternate_generation: self.alternate.generation(),
            cursor: grid.cursor(),
            offset: grid.offset(),
            view: grid.view(),
            modes: self.modes,
            scroll_region: self.scroll_region,
            title: self.title.clone(),
            palette: self.palette,
            default_colors: self.default_colors,
            image_metrics: self.images.metrics(),
            row_before,
            visible_before,
            scrollback_rows,
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "keeping action-level semantic diffing centralized prevents revision gaps"
    )]
    fn record_action_changes(&mut self, before: &ActionBaseline, hint: ActionHint) {
        let after_cursor = self.grid().cursor();
        let after_offset = self.grid().offset();
        let after_view = self.grid().view();
        let row_capacity = self.grid().row_capacity();
        let screen_rows = self.grid().screen_rows();
        let grid_changed = match self.active {
            ActiveScreen::Normal => self.normal.generation() != before.normal_generation,
            ActiveScreen::Alternate => self.alternate.generation() != before.alternate_generation,
        };
        let offset_changed = after_offset != before.offset;
        let view_changed = after_view != before.view;
        let row_changed = before.row_before.as_ref().is_some_and(|(row_number, row)| {
            self.grid()
                .row(*row_number)
                .is_some_and(|after| !rows_semantically_equal(after, row))
        });
        let visible_changed = before.visible_before.as_ref().is_some_and(|rows| {
            let after = self.grid().snapshot_view_rows();
            after.len() != rows.len()
                || after
                    .iter()
                    .zip(rows)
                    .any(|(left, right)| !rows_semantically_equal(left, right))
        });
        let scrollback_rows = before
            .scrollback_rows
            .map(|_| self.grid().snapshot_scrollback_rows(usize::MAX).1);

        let change = self
            .current_change
            .as_mut()
            .expect("change transaction active");
        if self.active != before.active || matches!(hint, ActionHint::Esc(b'c')) {
            change.full();
        } else if grid_changed {
            match hint {
                ActionHint::Print => {
                    let old_row = before.cursor.position().row;
                    let new_row = after_cursor.position().row;
                    let first = usize::try_from(old_row.min(new_row)).unwrap();
                    let last = usize::try_from(old_row.max(new_row)).unwrap() + 1;
                    change.rows(first, last.min(screen_rows));
                }
                ActionHint::Csi {
                    final_byte: b'K' | b'@' | b'P' | b'X',
                    ..
                } => {
                    if row_changed {
                        change.row(usize::try_from(before.cursor.position().row).unwrap());
                    }
                }
                ActionHint::Csi {
                    final_byte: b'J',
                    first_param: 3,
                    ..
                } => {
                    if scrollback_rows != before.scrollback_rows {
                        change.push(TerminalDamage::Scrollback);
                    }
                }
                ActionHint::Csi {
                    final_byte: b'J', ..
                } => {
                    if visible_changed {
                        change.rows(0, screen_rows);
                    }
                }
                ActionHint::Csi {
                    final_byte: b'L' | b'M',
                    ..
                } => {
                    change.rows(
                        usize::try_from(before.cursor.position().row).unwrap(),
                        usize::try_from(self.scroll_region.end()).unwrap(),
                    );
                }
                ActionHint::Csi {
                    final_byte: b'S' | b'T',
                    ..
                }
                | ActionHint::Execute(0x0a..=0x0c)
                | ActionHint::Esc(b'D' | b'E' | b'M') => {
                    change.rows(
                        usize::try_from(self.scroll_region.start()).unwrap(),
                        usize::try_from(self.scroll_region.end()).unwrap(),
                    );
                }
                ActionHint::Csi {
                    private: Some(b'?'),
                    final_byte: b'h' | b'l',
                    ..
                } if self.active != before.active => change.full(),
                _ => {}
            }
        }

        if offset_changed {
            let direction = match hint {
                ActionHint::Esc(b'M')
                | ActionHint::Csi {
                    final_byte: b'L' | b'T',
                    ..
                } => ScrollDirection::Reverse,
                _ => ScrollDirection::Forward,
            };
            let distance = if direction == ScrollDirection::Forward {
                after_offset.wrapping_sub(before.offset) & (row_capacity - 1)
            } else {
                before.offset.wrapping_sub(after_offset) & (row_capacity - 1)
            };
            let region = match hint {
                ActionHint::Csi {
                    final_byte: b'L' | b'M',
                    ..
                } => ScrollRegion::new(before.cursor.position().row, self.scroll_region.end()),
                _ => self.scroll_region,
            };
            change.push(TerminalDamage::Scroll {
                direction,
                region,
                rows: distance.min(screen_rows),
            });
            change.push(TerminalDamage::Scrollback);
        }
        if view_changed {
            change.push(TerminalDamage::Viewport);
        }
        if after_cursor != before.cursor {
            change.push(TerminalDamage::Cursor {
                old: before.cursor,
                new: after_cursor,
            });
        }
        if self.modes != before.modes || self.scroll_region != before.scroll_region {
            change.push(TerminalDamage::Modes);
        }
        if self.title != before.title {
            change.push(TerminalDamage::Title);
        }
        if self.palette != before.palette {
            change.push(TerminalDamage::Palette { index: None });
        }
        if self.default_colors != before.default_colors {
            change.push(TerminalDamage::Palette { index: None });
        }
        if self.images.metrics() != before.image_metrics {
            change.push(TerminalDamage::Images {
                screen: self.active,
            });
        }
    }

    fn commit_image_change(&mut self) {
        debug_assert!(self.current_change.is_none());
        let mut change = ChangeSet::default();
        change.push(TerminalDamage::Images {
            screen: self.active,
        });
        self.current_change = Some(change);
        self.commit_change();
    }

    fn commit_change(&mut self) {
        let Some(mut change) = self.current_change.take() else {
            return;
        };
        if self.synchronized_updates {
            self.synchronized_change
                .merge(change, self.config.event_limit);
            return;
        }
        if !self.synchronized_change.is_empty() {
            self.synchronized_change
                .merge(change, self.config.event_limit);
            change = std::mem::take(&mut self.synchronized_change);
        }
        if change.is_empty() {
            return;
        }
        self.revision = self.revision.next();
        self.update_history.push_back(TerminalUpdate::new(
            self.revision,
            change.damage,
            change.events,
        ));
        while self.update_history.len() > self.config.update_history_limit {
            self.update_history.pop_front();
        }
    }

    fn print(&mut self, character: char) {
        let width = UnicodeWidthChar::width(character).unwrap_or(0);
        if width == 0 {
            self.combine(character);
            return;
        }
        let columns = self.grid().columns();
        if width > columns {
            return;
        }

        let mut cursor = self.grid().cursor();
        if cursor.deferred_wrap() {
            if self.modes.auto_margin {
                let row = usize::try_from(cursor.position().row).expect("cursor row is valid");
                self.grid_mut()
                    .row_mut(cursor.position().row)
                    .expect("visible row")
                    .set_linebreak(false);
                cursor.set_deferred_wrap(false);
                cursor.set_position(Coordinate::new(0, i32::try_from(row).unwrap()));
                self.grid_mut().set_cursor(cursor);
                self.line_feed();
                cursor = self.grid().cursor();
            } else {
                cursor.set_deferred_wrap(false);
            }
        }

        let mut column = usize::try_from(cursor.position().column).expect("cursor column is valid");
        let mut write_width = width;
        if column + width > columns && self.modes.auto_margin {
            let row_number = cursor.position().row;
            let row_index = usize::try_from(row_number).expect("cursor row is valid");
            self.overwrite_image_cells(row_index, row_index + 1, column, columns);
            for pad in column..columns {
                self.grid_mut().row_mut(row_number).expect("visible row")[pad]
                    .set_content(CellContent::Spacer(0));
            }
            self.grid_mut()
                .row_mut(row_number)
                .expect("visible row")
                .set_linebreak(false);
            cursor.set_position(Coordinate::new(0, cursor.position().row));
            cursor.set_deferred_wrap(false);
            self.grid_mut().set_cursor(cursor);
            self.line_feed();
            cursor = self.grid().cursor();
            column = 0;
        } else if column + width > columns {
            // Foot clips a wide glyph at the margin when autowrap is disabled;
            // no continuation cell can be stored beyond the row.
            write_width = 1;
        }

        let row_number = cursor.position().row;
        let row_index = usize::try_from(row_number).expect("cursor row is valid");
        let overwrite_end = if self.modes.insert {
            columns
        } else {
            column + write_width
        };
        self.overwrite_image_cells(row_index, row_index + 1, column, overwrite_end);
        if self.modes.insert {
            let background = self.attributes.background();
            self.grid_mut()
                .row_mut(row_number)
                .expect("visible row")
                .insert_cells(column, write_width, background);
        }
        let attributes = self.attributes;
        let row = self.grid_mut().row_mut(row_number).expect("visible row");
        row.clear_wide_intersections(column..column + write_width);
        row[column].set_content(CellContent::Scalar(character));
        row[column].set_attributes(attributes);
        row[column].attributes_mut().set_clean(false);
        for offset in 1..write_width {
            row[column + offset].set_content(CellContent::Spacer(
                u32::try_from(write_width - offset).expect("character width fits in u32"),
            ));
            row[column + offset].set_attributes(Attributes::default());
        }
        row.set_linebreak(true);

        if column + write_width >= columns {
            cursor.set_position(Coordinate::new(
                i32::try_from(columns - 1).unwrap(),
                cursor.position().row,
            ));
            cursor.set_deferred_wrap(self.modes.auto_margin);
        } else {
            cursor.set_position(Coordinate::new(
                i32::try_from(column + write_width).unwrap(),
                cursor.position().row,
            ));
        }
        self.grid_mut().set_cursor(cursor);
    }

    fn combine(&mut self, character: char) {
        let cursor = self.grid().cursor();
        let column = usize::try_from(cursor.position().column).expect("cursor column is valid");
        let mut base_column = if cursor.deferred_wrap() {
            column
        } else if column > 0 {
            column - 1
        } else {
            return;
        };
        let row_number = cursor.position().row;
        while base_column > 0
            && matches!(
                self.grid().row(row_number).expect("visible row")[base_column].content(),
                CellContent::Spacer(remaining) if remaining > 0
            )
        {
            base_column -= 1;
        }
        let content = self.grid().row(row_number).expect("visible row")[base_column].content();
        let mut sequence = match content {
            CellContent::Scalar(base) => vec![base],
            CellContent::Composed(key) => self
                .composed
                .chars(key)
                .map_or_else(Vec::new, ToOwned::to_owned),
            _ => return,
        };
        if sequence.len() >= 64 {
            return;
        }
        sequence.push(character);
        if let Some(key) = self.composed.intern(sequence) {
            self.grid_mut().row_mut(row_number).expect("visible row")[base_column]
                .set_content(CellContent::Composed(key));
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => self.push_event(TerminalEvent::Bell),
            0x08 => self.backspace(),
            0x09 => self.tab(1, true),
            0x0a..=0x0c => self.line_feed(),
            0x0d => self.carriage_return(),
            _ => {}
        }
    }

    fn clear_deferred_wrap(&mut self, home_column: bool) {
        let mut cursor = self.grid().cursor();
        cursor.set_deferred_wrap(false);
        if home_column {
            cursor.set_position(Coordinate::new(0, cursor.position().row));
        }
        self.grid_mut().set_cursor(cursor);
    }

    fn carriage_return(&mut self) {
        let mut cursor = self.grid().cursor();
        cursor.set_position(Coordinate::new(0, cursor.position().row));
        cursor.set_deferred_wrap(false);
        self.grid_mut().set_cursor(cursor);
    }

    fn line_feed(&mut self) {
        let mut cursor = self.grid().cursor();
        cursor.set_deferred_wrap(false);
        let row = cursor.position().row;
        if row == self.scroll_region.end() - 1 {
            let background = self.attributes.background();
            let region = self.scroll_region;
            self.scroll_grid(ScrollDirection::Forward, region, 1, background);
        } else {
            let bottom = i32::try_from(self.grid().screen_rows() - 1).unwrap();
            cursor.set_position(Coordinate::new(
                cursor.position().column,
                (row + 1).min(bottom),
            ));
            self.grid_mut().set_cursor(cursor);
        }
    }

    fn reverse_index(&mut self) {
        let mut cursor = self.grid().cursor();
        cursor.set_deferred_wrap(false);
        if cursor.position().row == self.scroll_region.start() {
            let background = self.attributes.background();
            let region = self.scroll_region;
            self.scroll_grid(ScrollDirection::Reverse, region, 1, background);
        } else {
            cursor.set_position(Coordinate::new(
                cursor.position().column,
                (cursor.position().row - 1).max(0),
            ));
            self.grid_mut().set_cursor(cursor);
        }
    }

    fn backspace(&mut self) {
        let mut cursor = self.grid().cursor();
        if cursor.deferred_wrap() {
            cursor.set_deferred_wrap(false);
        } else if cursor.position().column > 0 {
            cursor.set_position(Coordinate::new(
                cursor.position().column - 1,
                cursor.position().row,
            ));
        } else if self.modes.reverse_wrap && cursor.position().row > 0 {
            cursor.set_position(Coordinate::new(
                i32::try_from(self.grid().columns() - 1).unwrap(),
                cursor.position().row - 1,
            ));
        }
        self.grid_mut().set_cursor(cursor);
    }

    fn tab(&mut self, count: usize, forward: bool) {
        let mut cursor = self.grid().cursor();
        let mut column = usize::try_from(cursor.position().column).unwrap();
        // Each step moves at least one column until an edge, so more steps than
        // columns cannot change the result.
        for _ in 0..count.min(self.tab_stops.len()) {
            if forward {
                column = ((column + 1)..self.tab_stops.len())
                    .find(|&candidate| self.tab_stops[candidate])
                    .unwrap_or(self.tab_stops.len() - 1);
            } else {
                column = (0..column)
                    .rev()
                    .find(|&candidate| self.tab_stops[candidate])
                    .unwrap_or(0);
            }
        }
        cursor.set_position(Coordinate::new(
            i32::try_from(column).unwrap(),
            cursor.position().row,
        ));
        cursor.set_deferred_wrap(false);
        self.grid_mut().set_cursor(cursor);
    }

    fn esc(&mut self, intermediates: &[u8], final_byte: u8) {
        if !intermediates.is_empty() {
            return;
        }
        match final_byte {
            b'7' => self.save_cursor(),
            b'8' => self.restore_cursor(),
            b'D' => self.line_feed(),
            b'E' => {
                self.carriage_return();
                self.line_feed();
            }
            b'H' => {
                let column = usize::try_from(self.grid().cursor().position().column).unwrap();
                self.tab_stops[column] = true;
            }
            b'M' => self.reverse_index(),
            b'c' => self.reset(),
            b'=' => self.modes.application_keypad = true,
            b'>' => self.modes.application_keypad = false,
            _ => {}
        }
    }

    fn csi(&mut self, private: Option<u8>, intermediates: &[u8], params: &Params, final_byte: u8) {
        if !intermediates.is_empty() {
            self.push_event(TerminalEvent::UnsupportedSequence("CSI intermediate"));
            return;
        }
        if private == Some(b'?') && matches!(final_byte, b'h' | b'l') {
            self.set_private_modes(params, final_byte == b'h');
            return;
        }
        // Cava 0.10.7 emits non-private CSI 2026 l/h followed by ST,
        // with reset beginning a frame and set ending it. Accept that exact
        // reversed mode as a narrow compatibility quirk; standard private
        // DECSET/DECRST semantics above remain unchanged.
        if private.is_none()
            && matches!(final_byte, b'h' | b'l')
            && params.count() == 1
            && params.get(0).value(0, false) == 2026
        {
            self.synchronized_updates = final_byte == b'l';
            return;
        }
        if private == Some(b'?') && final_byte == b'S' {
            self.xtsmgraphics(params);
            return;
        }
        if private.is_some()
            && !(final_byte == b'c' || (private == Some(b'?') && final_byte == b'n'))
        {
            return;
        }
        match final_byte {
            b'A' => self.move_cursor(0, -param_count(params.get(0))),
            b'B' | b'e' => self.move_cursor(0, param_count(params.get(0))),
            b'C' | b'a' => self.move_cursor(param_count(params.get(0)), 0),
            b'D' => self.move_cursor(-param_count(params.get(0)), 0),
            b'E' => {
                self.move_cursor(0, param_count(params.get(0)));
                self.carriage_return();
            }
            b'F' => {
                self.move_cursor(0, -param_count(params.get(0)));
                self.carriage_return();
            }
            b'G' | b'`' => self.set_column(params.get(0).value(1, true).saturating_sub(1)),
            b'd' => self.set_row(params.get(0).value(1, true).saturating_sub(1)),
            b'H' | b'f' => self.set_position(params),
            b'J' => self.erase_display(params.get(0).value(0, false)),
            b'K' => self.erase_line(params.get(0).value(0, false)),
            b'@' => self.insert_characters(params.get(0)),
            b'P' => self.delete_characters(params.get(0)),
            b'X' => self.erase_characters(params.get(0)),
            b'L' => self.insert_lines(params.get(0)),
            b'M' => self.delete_lines(params.get(0)),
            b'S' => self.scroll_lines(ScrollDirection::Forward, params.get(0)),
            b'T' => self.scroll_lines(ScrollDirection::Reverse, params.get(0)),
            b'I' => self.tab(param_usize(params.get(0)), true),
            b'Z' => self.tab(param_usize(params.get(0)), false),
            b'g' => self.clear_tabs(params.get(0).value(0, false)),
            b'h' | b'l' if params.get(0).value(0, false) == 4 => {
                self.modes.insert = final_byte == b'h';
            }
            b'm' => self.sgr(params),
            b'r' => self.set_scroll_region(params),
            b's' => self.save_cursor(),
            b'u' => self.restore_cursor(),
            b'n' => self.device_status(private, params.get(0).value(0, false)),
            b'c' => self.device_attributes(private),
            _ => {}
        }
    }

    fn xtsmgraphics(&mut self, params: &Params) {
        let item = params.get(0).value(0, false);
        let operation = params.get(1).value(0, false);
        match (item, operation) {
            (1, 1) => self.report_sixel_colors(self.sixel_palette_size),
            (1, 2) => {
                self.sixel_palette_size = MAX_SIXEL_COLORS;
                self.report_sixel_colors(self.sixel_palette_size);
            }
            (1, 3) => {
                self.sixel_palette_size = usize::try_from(params.get(2).value(0, false))
                    .unwrap_or(usize::MAX)
                    .clamp(2, MAX_SIXEL_COLORS);
                self.report_sixel_colors(self.sixel_palette_size);
            }
            (1, 4) => self.report_sixel_colors(MAX_SIXEL_COLORS),
            (2, 1) => self.report_sixel_geometry(false),
            (2, 2) => {
                self.sixel_maximum_width = self.config.image_limits.maximum_dimension;
                self.sixel_maximum_height = self.config.image_limits.maximum_dimension;
                self.report_sixel_geometry(false);
            }
            (2, 3) => {
                self.sixel_maximum_width = params
                    .get(2)
                    .value(0, false)
                    .min(self.config.image_limits.maximum_dimension);
                self.sixel_maximum_height = params
                    .get(3)
                    .value(0, false)
                    .min(self.config.image_limits.maximum_dimension);
                self.report_sixel_geometry(false);
            }
            (2, 4) => self.report_sixel_geometry(true),
            _ => {}
        }
    }

    fn report_sixel_colors(&mut self, count: usize) {
        self.push_event(TerminalEvent::PtyWrite(
            format!("\x1b[?1;0;{count}S").into_bytes(),
        ));
    }

    fn report_sixel_geometry(&mut self, maximum: bool) {
        let (width, height) = if maximum {
            (self.sixel_maximum_width, self.sixel_maximum_height)
        } else {
            let (cell_width, cell_height) = self.cell_pixels.unwrap_or((0, 0));
            let width = u32::try_from(self.grid().columns())
                .unwrap_or(u32::MAX)
                .saturating_mul(cell_width)
                .min(self.sixel_maximum_width);
            let height = u32::try_from(self.grid().screen_rows())
                .unwrap_or(u32::MAX)
                .saturating_mul(cell_height)
                .min(self.sixel_maximum_height);
            (width, height)
        };
        self.push_event(TerminalEvent::PtyWrite(
            format!("\x1b[?2;0;{width};{height}S").into_bytes(),
        ));
    }

    fn move_cursor(&mut self, column_delta: i32, row_delta: i32) {
        let mut cursor = self.grid().cursor();
        let position = cursor.position();
        let max_column = i32::try_from(self.grid().columns() - 1).unwrap();
        let (min_row, max_row) = if self.modes.origin {
            (self.scroll_region.start(), self.scroll_region.end() - 1)
        } else {
            (0, i32::try_from(self.grid().screen_rows() - 1).unwrap())
        };
        cursor.set_position(Coordinate::new(
            position
                .column
                .saturating_add(column_delta)
                .clamp(0, max_column),
            position
                .row
                .saturating_add(row_delta)
                .clamp(min_row, max_row),
        ));
        cursor.set_deferred_wrap(false);
        self.grid_mut().set_cursor(cursor);
    }

    fn set_column(&mut self, column: u32) {
        let mut cursor = self.grid().cursor();
        cursor.set_position(Coordinate::new(
            i32::try_from(column)
                .unwrap_or(i32::MAX)
                .min(i32::try_from(self.grid().columns() - 1).unwrap()),
            cursor.position().row,
        ));
        cursor.set_deferred_wrap(false);
        self.grid_mut().set_cursor(cursor);
    }

    fn set_row(&mut self, row: u32) {
        let mut cursor = self.grid().cursor();
        let base = if self.modes.origin {
            self.scroll_region.start()
        } else {
            0
        };
        let max = if self.modes.origin {
            self.scroll_region.end() - 1
        } else {
            i32::try_from(self.grid().screen_rows() - 1).unwrap()
        };
        cursor.set_position(Coordinate::new(
            cursor.position().column,
            base.saturating_add(i32::try_from(row).unwrap_or(i32::MAX))
                .min(max),
        ));
        cursor.set_deferred_wrap(false);
        self.grid_mut().set_cursor(cursor);
    }

    fn set_position(&mut self, params: &Params) {
        self.set_row(params.get(0).value(1, true).saturating_sub(1));
        self.set_column(params.get(1).value(1, true).saturating_sub(1));
    }

    #[allow(
        clippy::range_plus_one,
        reason = "the terminal erase API intentionally uses half-open Range values"
    )]
    fn erase_line(&mut self, mode: u32) {
        self.clear_deferred_wrap(false);
        let cursor = self.grid().cursor().position();
        let column = usize::try_from(cursor.column).unwrap();
        let columns = self.grid().columns();
        let background = self.attributes.background();
        let range = match mode {
            0 => column..columns,
            1 => 0..column + 1,
            2 => 0..columns,
            _ => return,
        };
        let row = usize::try_from(cursor.row).expect("cursor row is valid");
        self.overwrite_image_cells(row, row + 1, range.start, range.end);
        self.grid_mut()
            .row_mut(cursor.row)
            .expect("visible row")
            .erase(range, background);
    }

    #[allow(
        clippy::range_plus_one,
        reason = "the terminal erase API intentionally uses half-open Range values"
    )]
    fn erase_display(&mut self, mode: u32) {
        self.clear_deferred_wrap(false);
        let cursor = self.grid().cursor().position();
        let row = usize::try_from(cursor.row).unwrap();
        let column = usize::try_from(cursor.column).unwrap();
        let columns = self.grid().columns();
        let rows = self.grid().screen_rows();
        let background = self.attributes.background();
        match mode {
            0 => {
                self.overwrite_image_cells(row, row + 1, column, columns);
                self.overwrite_image_cells(row + 1, rows, 0, columns);
                self.grid_mut()
                    .row_mut(cursor.row)
                    .unwrap()
                    .erase(column..columns, background);
                for current in row + 1..rows {
                    self.grid_mut()
                        .row_mut(i32::try_from(current).unwrap())
                        .unwrap()
                        .erase_all(background);
                }
            }
            1 => {
                self.overwrite_image_cells(0, row, 0, columns);
                self.overwrite_image_cells(row, row + 1, 0, column + 1);
                for current in 0..row {
                    self.grid_mut()
                        .row_mut(i32::try_from(current).unwrap())
                        .unwrap()
                        .erase_all(background);
                }
                self.grid_mut()
                    .row_mut(cursor.row)
                    .unwrap()
                    .erase(0..column + 1, background);
            }
            2 => {
                self.overwrite_image_cells(0, rows, 0, columns);
                let ordered_rows = self.grid().ordered_retained_row_ids();
                let visible_rows = self
                    .grid()
                    .screen_row_ids()
                    .into_iter()
                    .collect::<HashSet<_>>();
                let kitty_placements = self
                    .images
                    .placements(self.active)
                    .filter(|placement| {
                        placement.erase_policy == ImageErasePolicy::ExplicitDelete
                            && placement_intersects_rows(placement, &ordered_rows, &visible_rows)
                    })
                    .map(|placement| placement.id)
                    .collect::<Vec<_>>();
                for placement_id in kitty_placements {
                    let _ = self.images.remove_placement(self.active, placement_id);
                }
                for current in 0..rows {
                    self.grid_mut()
                        .row_mut(i32::try_from(current).unwrap())
                        .unwrap()
                        .erase_all(background);
                }
            }
            3 => self.grid_mut().clear_scrollback(),
            _ => {}
        }
    }

    fn insert_characters(&mut self, param: Param) {
        self.clear_deferred_wrap(false);
        let cursor = self.grid().cursor().position();
        let row = usize::try_from(cursor.row).expect("cursor row is valid");
        let column = usize::try_from(cursor.column).expect("cursor column is valid");
        self.overwrite_image_cells(row, row + 1, column, self.grid().columns());
        let background = self.attributes.background();
        self.grid_mut().row_mut(cursor.row).unwrap().insert_cells(
            column,
            param_usize(param),
            background,
        );
    }

    fn delete_characters(&mut self, param: Param) {
        self.clear_deferred_wrap(false);
        let cursor = self.grid().cursor().position();
        let row = usize::try_from(cursor.row).expect("cursor row is valid");
        let column = usize::try_from(cursor.column).expect("cursor column is valid");
        self.overwrite_image_cells(row, row + 1, column, self.grid().columns());
        let background = self.attributes.background();
        self.grid_mut().row_mut(cursor.row).unwrap().delete_cells(
            column,
            param_usize(param),
            background,
        );
    }

    fn erase_characters(&mut self, param: Param) {
        self.clear_deferred_wrap(false);
        let cursor = self.grid().cursor().position();
        let start = usize::try_from(cursor.column).unwrap();
        let end = start
            .saturating_add(param_usize(param))
            .min(self.grid().columns());
        self.overwrite_image_cells(
            usize::try_from(cursor.row).expect("cursor row is valid"),
            usize::try_from(cursor.row).expect("cursor row is valid") + 1,
            start,
            end,
        );
        let background = self.attributes.background();
        self.grid_mut()
            .row_mut(cursor.row)
            .unwrap()
            .erase(start..end, background);
    }

    fn insert_lines(&mut self, param: Param) {
        self.clear_deferred_wrap(true);
        let row = self.grid().cursor().position().row;
        if row >= self.scroll_region.start() && row < self.scroll_region.end() {
            let region = ScrollRegion::new(row, self.scroll_region.end());
            let count =
                param_usize(param).min(usize::try_from(region.end() - region.start()).unwrap());
            let background = self.attributes.background();
            self.scroll_grid(ScrollDirection::Reverse, region, count, background);
        }
    }

    fn delete_lines(&mut self, param: Param) {
        self.clear_deferred_wrap(true);
        let row = self.grid().cursor().position().row;
        if row >= self.scroll_region.start() && row < self.scroll_region.end() {
            let region = ScrollRegion::new(row, self.scroll_region.end());
            let count =
                param_usize(param).min(usize::try_from(region.end() - region.start()).unwrap());
            let background = self.attributes.background();
            self.scroll_grid(ScrollDirection::Forward, region, count, background);
        }
    }

    fn scroll_lines(&mut self, direction: ScrollDirection, param: Param) {
        let height =
            usize::try_from(self.scroll_region.end() - self.scroll_region.start()).unwrap();
        let count = param_usize(param).min(height);
        let background = self.attributes.background();
        let region = self.scroll_region;
        self.scroll_grid(direction, region, count, background);
    }

    fn set_scroll_region(&mut self, params: &Params) {
        let rows = u32::try_from(self.grid().screen_rows()).unwrap();
        let start = params.get(0).value(1, true).saturating_sub(1).min(rows - 1);
        let end = params.get(1).value(rows, true).min(rows);
        if end > start + 1 {
            self.scroll_region =
                ScrollRegion::new(i32::try_from(start).unwrap(), i32::try_from(end).unwrap());
            let home = if self.modes.origin {
                self.scroll_region.start()
            } else {
                0
            };
            self.grid_mut()
                .set_cursor(Cursor::new(Coordinate::new(0, home)));
        }
    }

    fn save_cursor(&mut self) {
        let cursor = self.grid().cursor();
        self.grid_mut().set_saved_cursor(cursor);
        self.saved_attributes = self.attributes;
    }

    fn restore_cursor(&mut self) {
        let cursor = self.grid().saved_cursor();
        self.grid_mut().set_cursor(cursor);
        self.attributes = self.saved_attributes;
    }

    fn set_private_modes(&mut self, params: &Params, enabled: bool) {
        for index in 0..params.count().max(1) {
            match params.get(index).value(0, false) {
                1 => self.modes.application_cursor = enabled,
                5 => self.modes.reverse_video = enabled,
                6 => {
                    self.modes.origin = enabled;
                    let row = if enabled {
                        self.scroll_region.start()
                    } else {
                        0
                    };
                    self.grid_mut()
                        .set_cursor(Cursor::new(Coordinate::new(0, row)));
                }
                7 => {
                    self.modes.auto_margin = enabled;
                    let mut cursor = self.grid().cursor();
                    cursor.set_deferred_wrap(false);
                    self.grid_mut().set_cursor(cursor);
                }
                12 => self.modes.cursor_blink = enabled,
                25 => self.modes.cursor_visible = enabled,
                45 => self.modes.reverse_wrap = enabled,
                47 | 1047 => self.select_alternate(enabled, false),
                66 => self.modes.application_keypad = enabled,
                80 => self.sixel_scrolling = !enabled,
                1070 => {
                    self.sixel_palette_mode = if enabled {
                        SixelPaletteMode::Private
                    } else {
                        SixelPaletteMode::Shared
                    };
                }
                8452 => self.sixel_cursor_right = enabled,
                1000 => {
                    if enabled {
                        self.modes.mouse_tracking = MouseTracking::Normal;
                    } else if self.modes.mouse_tracking == MouseTracking::Normal {
                        self.modes.mouse_tracking = MouseTracking::None;
                    }
                }
                1002 => {
                    if enabled {
                        self.modes.mouse_tracking = MouseTracking::Button;
                    } else if self.modes.mouse_tracking == MouseTracking::Button {
                        self.modes.mouse_tracking = MouseTracking::None;
                    }
                }
                1003 => {
                    if enabled {
                        self.modes.mouse_tracking = MouseTracking::Any;
                    } else if self.modes.mouse_tracking == MouseTracking::Any {
                        self.modes.mouse_tracking = MouseTracking::None;
                    }
                }
                1006 => self.modes.sgr_mouse = enabled,
                1048 => {
                    if enabled {
                        self.save_cursor();
                    } else {
                        self.restore_cursor();
                    }
                }
                1049 => self.select_alternate(enabled, true),
                1004 => self.modes.focus_reporting = enabled,
                2004 => self.modes.bracketed_paste = enabled,
                2026 => self.synchronized_updates = enabled,
                _ => {}
            }
        }
    }

    fn select_alternate(&mut self, enabled: bool, save_restore: bool) {
        if enabled && self.active == ActiveScreen::Normal {
            if save_restore {
                self.save_cursor();
            }
            let normal_cursor = self.normal.cursor().position();
            let background = self.attributes.background();
            self.alternate.reset_visible(background);
            self.images.clear_screen(ActiveScreen::Alternate);
            self.kitty_image_ids.clear(ActiveScreen::Alternate);
            let alternate_position = clamp_position(&self.alternate, normal_cursor);
            self.alternate.set_cursor(Cursor::new(alternate_position));
            self.active = ActiveScreen::Alternate;
        } else if !enabled && self.active == ActiveScreen::Alternate {
            let alternate_cursor = self.alternate.cursor().position();
            self.active = ActiveScreen::Normal;
            let normal_position = clamp_position(&self.normal, alternate_cursor);
            self.normal.set_cursor(Cursor::new(normal_position));
            if save_restore {
                self.restore_cursor();
            }
        }
    }

    fn sgr(&mut self, params: &Params) {
        if params.count() == 0 {
            self.attributes = Attributes::default();
            return;
        }
        let mut index = 0;
        while index < params.count() {
            let code = params.get(index).value(0, false);
            match code {
                0 => self.attributes = Attributes::default(),
                1 => self.attributes.set_bold(true),
                2 => self.attributes.set_dim(true),
                3 => self.attributes.set_italic(true),
                4 => {
                    let style = match params.get(index).subparam(0).unwrap_or(1) {
                        0 => UnderlineStyle::None,
                        2 => UnderlineStyle::Double,
                        3 => UnderlineStyle::Curly,
                        4 => UnderlineStyle::Dotted,
                        5 => UnderlineStyle::Dashed,
                        _ => UnderlineStyle::Single,
                    };
                    self.attributes.set_underline_style(style);
                }
                21 => self.attributes.set_underline_style(UnderlineStyle::Double),
                5 => self.attributes.set_blink(true),
                7 => self.attributes.set_reverse(true),
                8 => self.attributes.set_conceal(true),
                9 => self.attributes.set_strikethrough(true),
                22 => {
                    self.attributes.set_bold(false);
                    self.attributes.set_dim(false);
                }
                23 => self.attributes.set_italic(false),
                24 => self.attributes.set_underline_style(UnderlineStyle::None),
                25 => self.attributes.set_blink(false),
                27 => self.attributes.set_reverse(false),
                28 => self.attributes.set_conceal(false),
                29 => self.attributes.set_strikethrough(false),
                30..=37 => self
                    .attributes
                    .set_foreground(Color::new(ColorSource::Base16, code - 30)),
                39 => self.attributes.set_foreground(Color::default()),
                59 => self.attributes.set_underline_color(Color::default()),
                40..=47 => self
                    .attributes
                    .set_background(Color::new(ColorSource::Base16, code - 40)),
                49 => self.attributes.set_background(Color::default()),
                90..=97 => self
                    .attributes
                    .set_foreground(Color::new(ColorSource::Base16, code - 90 + 8)),
                100..=107 => self
                    .attributes
                    .set_background(Color::new(ColorSource::Base16, code - 100 + 8)),
                38 | 48 | 58 => {
                    if let Some((color, consumed)) = extended_color(params, index) {
                        match code {
                            38 => self.attributes.set_foreground(color),
                            48 => self.attributes.set_background(color),
                            58 => self.attributes.set_underline_color(color),
                            _ => unreachable!(),
                        }
                        index += consumed;
                    }
                }
                _ => {}
            }
            index += 1;
        }
    }

    fn device_status(&mut self, private: Option<u8>, code: u32) {
        match (private, code) {
            (None, 5) => self.push_event(TerminalEvent::PtyWrite(b"\x1b[0n".to_vec())),
            (None | Some(b'?'), 6) => {
                let position = self.grid().cursor().position();
                let row = if self.modes.origin {
                    position.row - self.scroll_region.start()
                } else {
                    position.row
                } + 1;
                let prefix = if private == Some(b'?') { "?" } else { "" };
                self.push_event(TerminalEvent::PtyWrite(
                    format!("\x1b[{prefix}{row};{}R", position.column + 1).into_bytes(),
                ));
            }
            _ => {}
        }
    }

    fn device_attributes(&mut self, private: Option<u8>) {
        let reply = match private {
            None if self.config.sixel.enabled => Some(b"\x1b[?62;4;22c".to_vec()),
            None => Some(b"\x1b[?62;22c".to_vec()),
            Some(b'>') => Some(b"\x1b[>0;1;0c".to_vec()),
            _ => None,
        };
        if let Some(reply) = reply {
            self.push_event(TerminalEvent::PtyWrite(reply));
        }
    }

    fn clear_tabs(&mut self, mode: u32) {
        match mode {
            0 => {
                let column = usize::try_from(self.grid().cursor().position().column).unwrap();
                self.tab_stops[column] = false;
            }
            3 => self.tab_stops.fill(false),
            _ => {}
        }
    }

    fn osc(&mut self, payload: &[u8], terminator: StringTerminator) {
        let Ok(text) = std::str::from_utf8(payload) else {
            return;
        };
        let (command, data) = text.split_once(';').unwrap_or((text, ""));
        match command {
            "0" | "2" => {
                data.clone_into(&mut self.title);
                self.push_event(TerminalEvent::TitleChanged(self.title.clone()));
            }
            "4" => self.osc_palette(data, terminator),
            "52" => self.osc_clipboard(data, terminator),
            "10" | "11" | "12" => {
                let slot = command.parse::<usize>().unwrap() - 10;
                if data == "?" {
                    let reply = osc_color_reply(command, self.default_colors[slot], terminator);
                    self.push_event(TerminalEvent::PtyWrite(reply));
                } else if let Some(color) = parse_rgb(data) {
                    self.default_colors[slot] = color;
                    self.push_event(TerminalEvent::PaletteChanged {
                        index: u16::try_from(256 + slot).unwrap(),
                        color,
                    });
                }
            }
            "104" => {
                if data.is_empty() {
                    self.palette = self.initial_palette;
                } else {
                    for index in data
                        .split(';')
                        .filter_map(|value| value.parse::<usize>().ok())
                    {
                        if index < 256 {
                            self.palette[index] = self.initial_palette[index];
                            self.push_event(TerminalEvent::PaletteChanged {
                                index: u16::try_from(index).unwrap(),
                                color: self.palette[index],
                            });
                        }
                    }
                }
            }
            "110" | "111" | "112" => {
                let slot = command.parse::<usize>().unwrap() - 110;
                self.default_colors[slot] = self.initial_default_colors[slot];
                self.push_event(TerminalEvent::PaletteChanged {
                    index: u16::try_from(256 + slot).unwrap(),
                    color: self.default_colors[slot],
                });
            }
            _ => {}
        }
    }

    fn osc_palette(&mut self, data: &str, terminator: StringTerminator) {
        let mut fields = data.split(';');
        while let (Some(index), Some(spec)) = (fields.next(), fields.next()) {
            let Ok(index) = index.parse::<usize>() else {
                continue;
            };
            if index >= 256 {
                continue;
            }
            if spec == "?" {
                let color = self.palette[index];
                let suffix = match terminator {
                    StringTerminator::Bell => "\x07",
                    StringTerminator::StringTerminator => "\x1b\\",
                };
                let red = (color >> 16) & 0xff;
                let green = (color >> 8) & 0xff;
                let blue = color & 0xff;
                self.push_event(TerminalEvent::PtyWrite(
                    format!(
                        "\x1b]4;{index};rgb:{red:02x}{red:02x}/{green:02x}{green:02x}/{blue:02x}{blue:02x}{suffix}"
                    )
                    .into_bytes(),
                ));
            } else if let Some(color) = parse_rgb(spec) {
                self.palette[index] = color;
                self.push_event(TerminalEvent::PaletteChanged {
                    index: u16::try_from(index).unwrap(),
                    color,
                });
            }
        }
    }

    /// OSC 52 (Plan 0074 M2): `52;<targets>;<base64>` asks to set the named
    /// selections, and `52;<targets>;?` asks for one's contents. The terminal
    /// only reports either; it never touches a clipboard or answers a query.
    /// A set whose payload is empty, not base64, or over [`OSC52_MAX_BYTES`]
    /// decoded is ignored, as is a sequence that names no selection this
    /// terminal knows.
    fn osc_clipboard(&mut self, data: &str, terminator: StringTerminator) {
        let Some((targets, payload)) = data.split_once(';') else {
            return;
        };
        let selections = osc52_selections(targets);
        let Some(&first) = selections.first() else {
            return;
        };
        if payload == "?" {
            self.push_event(TerminalEvent::ClipboardQuery {
                selection: first,
                terminator: match terminator {
                    StringTerminator::Bell => ReplyTerminator::Bell,
                    StringTerminator::StringTerminator => ReplyTerminator::StringTerminator,
                },
            });
            return;
        }
        let Some(bytes) = decode_osc52(payload) else {
            return;
        };
        if let Some(&second) = selections.get(1) {
            self.push_event(TerminalEvent::ClipboardSet {
                selection: first,
                bytes: bytes.clone(),
            });
            self.push_event(TerminalEvent::ClipboardSet {
                selection: second,
                bytes,
            });
        } else {
            self.push_event(TerminalEvent::ClipboardSet {
                selection: first,
                bytes,
            });
        }
    }

    fn reset(&mut self) {
        let columns = self.grid().columns();
        let rows = self.grid().screen_rows();
        let config = self.config.clone();
        let normal_history_namespace = self.normal.history_namespace();
        let alternate_history_namespace = self.alternate.history_namespace();
        let cell_pixels = self.cell_pixels;
        let events = std::mem::take(&mut self.events);
        let event_overflowed = self.event_overflowed;
        let revision = self.revision;
        let update_history = std::mem::take(&mut self.update_history);
        let current_change = self.current_change.take();
        *self = Self::new(columns, rows, config);
        self.normal
            .continue_history_namespace(normal_history_namespace);
        self.alternate
            .continue_history_namespace(alternate_history_namespace);
        self.cell_pixels = cell_pixels;
        self.events = events;
        self.event_overflowed = event_overflowed;
        self.revision = revision;
        self.update_history = update_history;
        self.current_change = current_change;
    }

    fn reset_tab_stops(&mut self, columns: usize) {
        self.tab_stops = (0..columns)
            .map(|column| column > 0 && column % self.config.tab_width == 0)
            .collect();
    }
}

/// One search query matched against rows, with buffers reused across them.
struct RowSearch {
    needle: String,
    case_sensitive: bool,
    /// A match can reach past a row's last written cell only through blanks
    /// the query itself ends with, so a row is read that far past it. Search
    /// time then follows the row's content, not its width.
    trailing_blanks: usize,
    original: String,
    searchable: String,
    byte_columns: Vec<(usize, usize)>,
}

impl RowSearch {
    fn new(needle: String, case_sensitive: bool) -> Self {
        let trailing_blanks = needle.chars().rev().take_while(|&c| c == ' ').count();
        Self {
            needle,
            case_sensitive,
            trailing_blanks,
            original: String::new(),
            searchable: String::new(),
            byte_columns: Vec::new(),
        }
    }

    /// The first match in `cells`: its start and end columns and the row's
    /// text as a preview.
    fn find(
        &mut self,
        cells: &[crate::Cell],
        composed: &ComposedTable,
    ) -> Option<(usize, usize, String)> {
        let written = cells
            .iter()
            .rposition(|cell| !matches!(cell.content(), CellContent::Empty))
            .map_or(0, |last| last + 1);
        let cells = &cells[..written
            .saturating_add(self.trailing_blanks)
            .min(cells.len())];
        self.original.clear();
        self.searchable.clear();
        self.byte_columns.clear();
        for (column, cell) in cells.iter().enumerate() {
            let end_column = cells
                .get(column + 1)
                .and_then(|next| match next.content() {
                    CellContent::Spacer(remaining) => usize::try_from(remaining)
                        .ok()
                        .map(|remaining| column + remaining + 1),
                    _ => None,
                })
                .unwrap_or(column + 1);
            let mut push = |character: char| {
                self.original.push(character);
                let before = self.searchable.len();
                if self.case_sensitive {
                    self.searchable.push(character);
                } else {
                    self.searchable.extend(character.to_lowercase());
                }
                self.byte_columns.extend(std::iter::repeat_n(
                    (column, end_column),
                    self.searchable.len() - before,
                ));
            };
            match cell.content() {
                CellContent::Empty => push(' '),
                CellContent::Scalar(character) => push(character),
                CellContent::Composed(key) => {
                    for &character in composed.chars(key).unwrap_or_default() {
                        push(character);
                    }
                }
                CellContent::Spacer(_) => {}
            }
        }
        let start = self.searchable.find(&self.needle)?;
        let end = start + self.needle.len();
        let (start_column, _) = self.byte_columns.get(start).copied()?;
        let (_, end_column) = self.byte_columns.get(end.saturating_sub(1)).copied()?;
        let mut preview = self.original.trim_end().to_owned();
        while preview.len() > 256 {
            preview.pop();
        }
        Some((start_column, end_column, preview))
    }
}

fn rows_semantically_equal(left: &crate::Row, right: &crate::Row) -> bool {
    left.has_linebreak() == right.has_linebreak()
        && left.len() == right.len()
        && left.cells().iter().zip(right.cells()).all(|(left, right)| {
            let left_attributes = left.attributes();
            let right_attributes = right.attributes();
            left.content() == right.content()
                && left_attributes.bold() == right_attributes.bold()
                && left_attributes.dim() == right_attributes.dim()
                && left_attributes.italic() == right_attributes.italic()
                && left_attributes.underline_style() == right_attributes.underline_style()
                && left_attributes.underline_color() == right_attributes.underline_color()
                && left_attributes.strikethrough() == right_attributes.strikethrough()
                && left_attributes.blink() == right_attributes.blink()
                && left_attributes.conceal() == right_attributes.conceal()
                && left_attributes.reverse() == right_attributes.reverse()
                && left_attributes.foreground() == right_attributes.foreground()
                && left_attributes.background() == right_attributes.background()
        })
}

fn clamp_position(grid: &Grid, position: Coordinate) -> Coordinate {
    Coordinate::new(
        position
            .column
            .clamp(0, i32::try_from(grid.columns() - 1).unwrap()),
        position
            .row
            .clamp(0, i32::try_from(grid.screen_rows() - 1).unwrap()),
    )
}

fn param_count(param: Param) -> i32 {
    i32::try_from(param.value(1, true)).unwrap_or(i32::MAX)
}

fn param_usize(param: Param) -> usize {
    usize::try_from(param.value(1, true)).unwrap_or(usize::MAX)
}

fn extended_color(params: &Params, index: usize) -> Option<(Color, usize)> {
    let parameter = params.get(index);
    if parameter.subparam_count() > 0 {
        return match parameter.subparam(0)? {
            5 => Some((
                Color::new(ColorSource::Base256, parameter.subparam(1)?.min(255)),
                0,
            )),
            2 => {
                let red = parameter.subparam(2)?.min(255);
                let green = parameter.subparam(3)?.min(255);
                let blue = parameter.subparam(4)?.min(255);
                Some((Color::rgb((red << 16) | (green << 8) | blue), 0))
            }
            _ => None,
        };
    }
    match params.get(index + 1).value(0, false) {
        5 if index + 2 < params.count() => Some((
            Color::new(
                ColorSource::Base256,
                params.get(index + 2).value(0, false).min(255),
            ),
            2,
        )),
        2 if index + 4 < params.count() => {
            let red = params.get(index + 2).value(0, false).min(255);
            let green = params.get(index + 3).value(0, false).min(255);
            let blue = params.get(index + 4).value(0, false).min(255);
            Some((Color::rgb((red << 16) | (green << 8) | blue), 4))
        }
        _ => None,
    }
}

/// The selections an OSC 52 target list names, in first-named order and at
/// most once each: `c` the clipboard, `p` and `s` the primary selection (as
/// foot and Alacritty read `s`), and an empty list the clipboard. Cut
/// buffers `0`-`7`, `q`, and anything else name nothing here.
fn osc52_selections(targets: &str) -> Vec<ClipboardSelection> {
    if targets.is_empty() {
        return vec![ClipboardSelection::Clipboard];
    }
    let mut selections = Vec::with_capacity(2);
    for target in targets.bytes() {
        let selection = match target {
            b'c' => ClipboardSelection::Clipboard,
            b'p' | b's' => ClipboardSelection::Primary,
            _ => continue,
        };
        if !selections.contains(&selection) {
            selections.push(selection);
        }
    }
    selections
}

/// An OSC 52 payload decoded: standard base64, padded or not. `None` for
/// anything empty, malformed, or over [`OSC52_MAX_BYTES`] decoded.
fn decode_osc52(payload: &str) -> Option<Vec<u8>> {
    use base64::{
        Engine as _,
        alphabet::STANDARD,
        engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
    };
    const LENIENT: GeneralPurpose = GeneralPurpose::new(
        &STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    if payload.is_empty() || payload.len() > crate::OSC52_MAX_BASE64_BYTES {
        return None;
    }
    let bytes = LENIENT.decode(payload).ok()?;
    (!bytes.is_empty() && bytes.len() <= OSC52_MAX_BYTES).then_some(bytes)
}

fn osc_color_reply(command: &str, color: u32, terminator: StringTerminator) -> Vec<u8> {
    let suffix = match terminator {
        StringTerminator::Bell => "\x07",
        StringTerminator::StringTerminator => "\x1b\\",
    };
    let red = (color >> 16) & 0xff;
    let green = (color >> 8) & 0xff;
    let blue = color & 0xff;
    format!(
        "\x1b]{command};rgb:{red:02x}{red:02x}/{green:02x}{green:02x}/{blue:02x}{blue:02x}{suffix}"
    )
    .into_bytes()
}

fn parse_rgb(spec: &str) -> Option<u32> {
    if let Some(hex) = spec.strip_prefix('#') {
        return match hex.len() {
            3 => {
                let value = u32::from_str_radix(hex, 16).ok()?;
                Some(
                    ((value & 0xf00) << 12)
                        | ((value & 0x0f0) << 8)
                        | ((value & 0x00f) << 4)
                        | ((value & 0xf00) << 8)
                        | ((value & 0x0f0) << 4)
                        | (value & 0x00f),
                )
            }
            6 => u32::from_str_radix(hex, 16).ok(),
            _ => None,
        };
    }
    let rgb = spec.strip_prefix("rgb:")?;
    let mut components = rgb.split('/');
    let component = |value: &str| -> Option<u32> {
        if value.is_empty() || value.len() > 4 {
            return None;
        }
        let raw = u32::from_str_radix(value, 16).ok()?;
        let max = (1_u32 << (value.len() * 4)) - 1;
        Some((raw * 255 + max / 2) / max)
    };
    let red = component(components.next()?)?;
    let green = component(components.next()?)?;
    let blue = component(components.next()?)?;
    Some((red << 16) | (green << 8) | blue)
}

fn sixel_palette(initial: &[u32]) -> Box<[u32; MAX_SIXEL_COLORS]> {
    let mut palette = Box::new([0; MAX_SIXEL_COLORS]);
    let count = initial.len().min(MAX_SIXEL_COLORS);
    palette[..count].copy_from_slice(&initial[..count]);
    palette
}

fn iterm_destination_extent(
    command: iterm::Command,
    source_width: u32,
    source_height: u32,
    cell_width: u32,
    cell_height: u32,
    screen_columns: usize,
    screen_rows: usize,
) -> Result<(usize, usize), KittyError> {
    let requested = |extent: iterm::Extent,
                     source: u32,
                     cell: u32,
                     screen_cells: usize|
     -> Result<u128, KittyError> {
        match extent {
            iterm::Extent::Auto => Ok(u128::from(source)),
            iterm::Extent::Cells(cells) => u128::try_from(cells)
                .unwrap_or(u128::MAX)
                .checked_mul(u128::from(cell))
                .ok_or(KittyError::TooBig("iTerm2 destination extent overflow")),
            iterm::Extent::Pixels(pixels) => Ok(u128::from(pixels)),
            iterm::Extent::Percent(percent) => u128::try_from(screen_cells)
                .unwrap_or(u128::MAX)
                .checked_mul(u128::from(cell))
                .and_then(|value| value.checked_mul(u128::from(percent)))
                .and_then(|value| value.checked_add(99))
                .map(|value| value / 100)
                .ok_or(KittyError::TooBig("iTerm2 destination extent overflow")),
        }
    };
    let mut width = requested(command.width, source_width, cell_width, screen_columns)?;
    let mut height = requested(command.height, source_height, cell_height, screen_rows)?;
    if command.preserve_aspect_ratio {
        match (command.width, command.height) {
            (iterm::Extent::Auto, iterm::Extent::Auto) => {}
            (iterm::Extent::Auto, _) => {
                width = height
                    .checked_mul(u128::from(source_width))
                    .and_then(|value| {
                        value
                            .checked_add(u128::from(source_height) - 1)
                            .map(|value| value / u128::from(source_height))
                    })
                    .ok_or(KittyError::TooBig("iTerm2 destination extent overflow"))?;
            }
            (_, iterm::Extent::Auto) => {
                height = width
                    .checked_mul(u128::from(source_height))
                    .and_then(|value| {
                        value
                            .checked_add(u128::from(source_width) - 1)
                            .map(|value| value / u128::from(source_width))
                    })
                    .ok_or(KittyError::TooBig("iTerm2 destination extent overflow"))?;
            }
            (_, _) => {
                if width
                    .checked_mul(u128::from(source_height))
                    .ok_or(KittyError::TooBig("iTerm2 destination extent overflow"))?
                    <= height
                        .checked_mul(u128::from(source_width))
                        .ok_or(KittyError::TooBig("iTerm2 destination extent overflow"))?
                {
                    height = width
                        .checked_mul(u128::from(source_height))
                        .and_then(|value| {
                            value
                                .checked_add(u128::from(source_width) - 1)
                                .map(|value| value / u128::from(source_width))
                        })
                        .ok_or(KittyError::TooBig("iTerm2 destination extent overflow"))?;
                } else {
                    width = height
                        .checked_mul(u128::from(source_width))
                        .and_then(|value| {
                            value
                                .checked_add(u128::from(source_height) - 1)
                                .map(|value| value / u128::from(source_height))
                        })
                        .ok_or(KittyError::TooBig("iTerm2 destination extent overflow"))?;
                }
            }
        }
    }
    let cell_width = u128::from(cell_width);
    let cell_height = u128::from(cell_height);
    let columns = width
        .checked_add(cell_width - 1)
        .map(|value| value / cell_width)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or(KittyError::TooBig("iTerm2 destination extent overflow"))?;
    let rows = height
        .checked_add(cell_height - 1)
        .map(|value| value / cell_height)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or(KittyError::TooBig("iTerm2 destination extent overflow"))?;
    Ok((columns, rows))
}

fn kitty_aspect_extent(
    source_axis: u32,
    fixed_cells: usize,
    fixed_cell_pixels: u32,
    fixed_source_axis: u32,
    derived_cell_pixels: u32,
) -> Result<usize, KittyError> {
    if fixed_cells == 0 || fixed_source_axis == 0 || derived_cell_pixels == 0 {
        return Err(KittyError::Invalid("destination extent must be non-zero"));
    }
    let numerator = u128::from(source_axis)
        .checked_mul(u128::try_from(fixed_cells).unwrap_or(u128::MAX))
        .and_then(|value| value.checked_mul(u128::from(fixed_cell_pixels)))
        .ok_or(KittyError::TooBig("destination extent overflow"))?;
    let denominator = u128::from(fixed_source_axis)
        .checked_mul(u128::from(derived_cell_pixels))
        .ok_or(KittyError::TooBig("destination extent overflow"))?;
    let extent = numerator
        .checked_add(denominator - 1)
        .map(|value| value / denominator)
        .ok_or(KittyError::TooBig("destination extent overflow"))?;
    usize::try_from(extent.max(1)).map_err(|_| KittyError::TooBig("destination extent overflow"))
}

fn placement_intersects_rows(
    placement: &ImagePlacement,
    ordered_rows: &[u64],
    visible_rows: &HashSet<u64>,
) -> bool {
    ordered_rows
        .iter()
        .position(|row_id| *row_id == placement.row_id)
        .is_some_and(|start| {
            ordered_rows
                .iter()
                .skip(start)
                .take(placement.destination.rows)
                .any(|row_id| visible_rows.contains(row_id))
        })
}

fn kitty_continuation_control(control: &[u8]) -> bool {
    let mut has_more = false;
    let valid = control.split(|byte| *byte == b',').all(|field| {
        field.split_first().is_some_and(|(key, rest)| {
            has_more |= *key == b'm';
            matches!(*key, b'm' | b'q') && rest.first() == Some(&b'=')
        })
    });
    valid && has_more
}

fn kitty_correlation(control: &[u8]) -> KittyCorrelation {
    let mut correlation = KittyCorrelation {
        image_id: 0,
        placement_id: 0,
        quiet: 0,
    };
    for field in control.split(|byte| *byte == b',') {
        let Some(separator) = field.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let value = &field[separator + 1..];
        let Ok(value) = std::str::from_utf8(value)
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .ok_or(())
        else {
            continue;
        };
        match field.first().copied() {
            Some(b'i') => correlation.image_id = value,
            Some(b'p') => correlation.placement_id = value,
            Some(b'q') => correlation.quiet = u8::try_from(value).unwrap_or(u8::MAX),
            _ => {}
        }
    }
    correlation
}

const fn map_image_error(error: ImageError) -> KittyError {
    match error {
        ImageError::UnknownContent | ImageError::UnknownPlacement => {
            KittyError::NotFound("image object does not exist")
        }
        ImageError::Dimensions
        | ImageError::PixelLength
        | ImageError::InvalidAnchor
        | ImageError::InvalidCrop
        | ImageError::InvalidDestination => KittyError::Invalid("invalid image geometry"),
        ImageError::ContentBytes
        | ImageError::TerminalBytes
        | ImageError::DaemonBytes
        | ImageError::ContentCount
        | ImageError::PlacementCount => KittyError::NoSpace("image resource limit reached"),
        ImageError::IdentityExhausted => KittyError::NoSpace("image identity exhausted"),
    }
}

fn default_palette() -> [u32; 256] {
    let mut palette = [0; 256];
    let base = [
        0x0000_0000,
        0x0080_0000,
        0x0000_8000,
        0x0080_8000,
        0x0000_0080,
        0x0080_0080,
        0x0000_8080,
        0x00c0_c0c0,
        0x0080_8080,
        0x00ff_0000,
        0x0000_ff00,
        0x00ff_ff00,
        0x0000_00ff,
        0x00ff_00ff,
        0x0000_ffff,
        0x00ff_ffff,
    ];
    palette[..16].copy_from_slice(&base);
    for (index, color) in palette.iter_mut().enumerate().take(232).skip(16) {
        let value = index - 16;
        let component = |part: usize| if part == 0 { 0 } else { 55 + part * 40 };
        let red = component(value / 36);
        let green = component((value / 6) % 6);
        let blue = component(value % 6);
        *color = u32::try_from((red << 16) | (green << 8) | blue).unwrap();
    }
    for (offset, color) in palette[232..].iter_mut().enumerate() {
        let value = 8 + offset * 10;
        *color = u32::try_from((value << 16) | (value << 8) | value).unwrap();
    }
    palette
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal(columns: usize, rows: usize) -> Terminal {
        Terminal::new(columns, rows, TerminalConfig::default())
    }

    fn row_text(terminal: &Terminal, row: i32) -> String {
        terminal
            .grid()
            .row(row)
            .unwrap()
            .cells()
            .iter()
            .map(|cell| match cell.content() {
                CellContent::Empty | CellContent::Spacer(_) => ' ',
                CellContent::Scalar(character) => character,
                CellContent::Composed(_) => '◌',
            })
            .collect()
    }

    #[test]
    fn existing_oracle_print_and_wrap_semantics_match() {
        let mut terminal = terminal(5, 3);
        terminal.advance(b"abcdef");

        assert_eq!(row_text(&terminal, 0), "abcde");
        assert_eq!(row_text(&terminal, 1), "f    ");
        assert!(!terminal.grid().row(0).unwrap().has_linebreak());
        assert_eq!(terminal.grid().cursor().position(), Coordinate::new(1, 1));
    }

    // The MCP text form (Plan 0067 M2) joins a row that has content and no
    // linebreak to the next row. A row starts with a hard linebreak (true),
    // printing keeps it, and only an autowrap clears it, on the row the text
    // wrapped from. A full row ended by CR LF is not a wrap.
    #[test]
    fn linebreak_is_a_hard_line_end_that_only_an_autowrap_clears() {
        let mut terminal = terminal(5, 6);
        terminal.advance(b"abcdefg\r\nhi\r\n\r\n12345\r\nz");

        let rows = (0..6)
            .map(|row| {
                let text = row_text(&terminal, row);
                let linebreak = terminal.grid().row(row).unwrap().has_linebreak();
                (text, linebreak)
            })
            .collect::<Vec<_>>();
        let expected = [
            ("abcde", false),
            ("fg   ", true),
            ("hi   ", true),
            ("     ", true),
            ("12345", true),
            ("z    ", true),
        ]
        .map(|(text, linebreak)| (text.to_owned(), linebreak));
        assert_eq!(rows, expected);
    }

    #[test]
    fn csi_cursor_erase_and_basic_sgr_match_initial_fixtures() {
        let mut terminal = terminal(8, 3);
        terminal.advance(b"abcdef\x1b[3D\x1b[K");
        assert_eq!(row_text(&terminal, 0), "abc     ");
        assert_eq!(terminal.grid().cursor().position(), Coordinate::new(3, 0));

        terminal.advance(b"\r\x1b[1;31mR\x1b[0mN");
        let row = terminal.grid().row(0).unwrap();
        assert!(row[0].attributes().bold());
        assert_eq!(
            row[0].attributes().foreground(),
            Color::new(ColorSource::Base16, 1)
        );
        assert_eq!(row[1].attributes(), Attributes::default());
    }

    #[test]
    fn mouse_tracking_reset_only_clears_the_matching_active_mode() {
        let mut terminal = terminal(6, 2);
        terminal.advance(b"\x1b[?1002h\x1b[?1000l");
        assert_eq!(terminal.modes.mouse_tracking, MouseTracking::Button);
        terminal.advance(b"\x1b[?1003h\x1b[?1002l");
        assert_eq!(terminal.modes.mouse_tracking, MouseTracking::Any);
        terminal.advance(b"\x1b[?1003l");
        assert_eq!(terminal.modes.mouse_tracking, MouseTracking::None);
        terminal.advance(b"\x1b[?1000h\x1b[?1000l");
        assert_eq!(terminal.modes.mouse_tracking, MouseTracking::None);
    }

    #[test]
    fn utf8_wide_and_combining_input_remains_valid() {
        let mut terminal = terminal(6, 2);
        terminal.advance("界e\u{301}".as_bytes());

        let row = terminal.grid().row(0).unwrap();
        assert_eq!(row[0].content(), CellContent::Scalar('界'));
        assert_eq!(row[1].content(), CellContent::Spacer(1));
        assert!(matches!(row[2].content(), CellContent::Composed(_)));
        assert!(row.has_valid_wide_cells());
    }

    #[test]
    fn c0_controls_and_bell_update_state_in_order() {
        let mut terminal = terminal(10, 3);
        terminal.advance(b"ab\x08Z\rQ\n\tX\x07");

        assert_eq!(row_text(&terminal, 0), "QZ        ");
        assert_eq!(row_text(&terminal, 1), "        X ");
        assert_eq!(
            terminal.drain_events().collect::<Vec<_>>(),
            vec![TerminalEvent::Bell]
        );
    }

    #[test]
    fn every_single_split_matches_whole_buffer() {
        let input = b"abc\x1b[2;3HZ\x1b[38;2;12;34;56mR\x1b]2;title\x07";
        let mut expected = terminal(10, 4);
        expected.advance(input);

        for split in 0..=input.len() {
            let mut actual = terminal(10, 4);
            actual.advance(&input[..split]);
            actual.advance(&input[split..]);
            assert_eq!(actual, expected, "split at {split}");
        }

        let mut bytewise = terminal(10, 4);
        for byte in input {
            bytewise.advance(std::slice::from_ref(byte));
        }
        assert_eq!(bytewise, expected);
    }

    #[test]
    fn sgr_colon_underline_styles_and_reset_match_foot() {
        let mut terminal = terminal(6, 1);
        terminal.advance(b"\x1b[4:1mA\x1b[4:2mB\x1b[4:3mC\x1b[4:4mD\x1b[4:5mE\x1b[24mF");
        let row = terminal.grid().row(0).unwrap();
        assert_eq!(
            row[0].attributes().underline_style(),
            UnderlineStyle::Single
        );
        assert_eq!(
            row[1].attributes().underline_style(),
            UnderlineStyle::Double
        );
        assert_eq!(row[2].attributes().underline_style(), UnderlineStyle::Curly);
        assert_eq!(
            row[3].attributes().underline_style(),
            UnderlineStyle::Dotted
        );
        assert_eq!(
            row[4].attributes().underline_style(),
            UnderlineStyle::Dashed
        );
        assert_eq!(row[5].attributes().underline_style(), UnderlineStyle::None);
    }

    #[test]
    fn sgr_underline_color_set_and_reset_are_independent() {
        let mut terminal = terminal(2, 1);
        terminal.advance(b"\x1b[4:3;58;2;12;34;56mA\x1b[59mB");
        let row = terminal.grid().row(0).unwrap();
        assert_eq!(
            row[0].attributes().underline_color(),
            Color::rgb(0x000c_2238)
        );
        assert_eq!(row[1].attributes().underline_color(), Color::default());
        assert_eq!(row[1].attributes().underline_style(), UnderlineStyle::Curly);
    }

    #[test]
    fn scroll_region_and_line_feed_preserve_outside_rows() {
        let mut terminal = terminal(4, 4);
        terminal.advance(b"top\r\n111\r\n222\r\nend");
        terminal.advance(b"\x1b[2;3r\x1b[3;1HX\n");

        assert!(row_text(&terminal, 0).starts_with("top"));
        assert!(row_text(&terminal, 3).starts_with("end"));
    }

    #[test]
    fn extended_sgr_colors_support_indexed_rgb_and_colon_forms() {
        let mut terminal = terminal(6, 1);
        terminal.advance(b"\x1b[38;5;200mA\x1b[48;2;1;2;3mB\x1b[38:2::4:5:6mC");
        let row = terminal.grid().row(0).unwrap();
        assert_eq!(
            row[0].attributes().foreground(),
            Color::new(ColorSource::Base256, 200)
        );
        assert_eq!(row[1].attributes().background(), Color::rgb(0x01_02_03));
        assert_eq!(row[2].attributes().foreground(), Color::rgb(0x04_05_06));
    }

    #[test]
    fn queries_emit_ordered_pty_replies() {
        let mut terminal = terminal(6, 3);
        terminal.advance(b"\x1b[2;3H\x1b[5n\x1b[6n\x1b[c");
        assert_eq!(
            terminal.drain_events().collect::<Vec<_>>(),
            vec![
                TerminalEvent::PtyWrite(b"\x1b[0n".to_vec()),
                TerminalEvent::PtyWrite(b"\x1b[2;3R".to_vec()),
                TerminalEvent::PtyWrite(b"\x1b[?62;4;22c".to_vec()),
            ]
        );
    }

    #[test]
    fn alternate_screen_restores_normal_content_and_cursor() {
        let mut terminal = terminal(6, 2);
        terminal.advance(b"normal\x1b[?1049halt\x1b[?1049l");

        assert_eq!(terminal.active_screen(), ActiveScreen::Normal);
        assert_eq!(row_text(&terminal, 0), "normal");
        assert_eq!(terminal.grid().cursor().position(), Coordinate::new(5, 0));
    }

    fn clipboard_events(terminal: &mut Terminal) -> Vec<TerminalEvent> {
        terminal
            .drain_events()
            .filter(|event| {
                matches!(
                    event,
                    TerminalEvent::ClipboardSet { .. } | TerminalEvent::ClipboardQuery { .. }
                )
            })
            .collect()
    }

    fn clipboard_set(selection: ClipboardSelection, bytes: &[u8]) -> TerminalEvent {
        TerminalEvent::ClipboardSet {
            selection,
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn osc52_sets_each_target_and_an_empty_target_means_the_clipboard() {
        let mut terminal = terminal(8, 2);
        // "hi" is aGk= in base64.
        for (targets, expected) in [
            ("c", vec![ClipboardSelection::Clipboard]),
            ("", vec![ClipboardSelection::Clipboard]),
            ("p", vec![ClipboardSelection::Primary]),
            ("s", vec![ClipboardSelection::Primary]),
            (
                "pc",
                vec![ClipboardSelection::Primary, ClipboardSelection::Clipboard],
            ),
            (
                "cc0s",
                vec![ClipboardSelection::Clipboard, ClipboardSelection::Primary],
            ),
        ] {
            terminal.advance(format!("\x1b]52;{targets};aGk=\x07").as_bytes());
            let expected = expected
                .into_iter()
                .map(|selection| clipboard_set(selection, b"hi"))
                .collect::<Vec<_>>();
            assert_eq!(clipboard_events(&mut terminal), expected, "{targets:?}");
        }
        // Unpadded base64 is accepted too, and the ST terminator works.
        terminal.advance(b"\x1b]52;c;aGk\x1b\\");
        assert_eq!(
            clipboard_events(&mut terminal),
            vec![clipboard_set(ClipboardSelection::Clipboard, b"hi")]
        );
        // The terminal never writes anything back for a set.
        assert!(
            !terminal
                .drain_events()
                .any(|event| matches!(event, TerminalEvent::PtyWrite(_)))
        );
    }

    #[test]
    fn osc52_queries_report_the_target_and_terminator_and_are_never_answered() {
        let mut terminal = terminal(8, 2);
        for (sequence, selection, terminator) in [
            (
                &b"\x1b]52;c;?\x07"[..],
                ClipboardSelection::Clipboard,
                ReplyTerminator::Bell,
            ),
            (
                &b"\x1b]52;;?\x1b\\"[..],
                ClipboardSelection::Clipboard,
                ReplyTerminator::StringTerminator,
            ),
            (
                &b"\x1b]52;p;?\x07"[..],
                ClipboardSelection::Primary,
                ReplyTerminator::Bell,
            ),
            (
                &b"\x1b]52;s;?\x1b\\"[..],
                ClipboardSelection::Primary,
                ReplyTerminator::StringTerminator,
            ),
            // A query names one selection: the first.
            (
                &b"\x1b]52;pc;?\x07"[..],
                ClipboardSelection::Primary,
                ReplyTerminator::Bell,
            ),
        ] {
            terminal.advance(sequence);
            let events = terminal.drain_events().collect::<Vec<_>>();
            assert_eq!(
                events,
                vec![TerminalEvent::ClipboardQuery {
                    selection,
                    terminator,
                }],
                "{sequence:?}"
            );
        }
        assert_eq!(
            crate::osc52_reply(ClipboardSelection::Clipboard, b"hi", ReplyTerminator::Bell),
            b"\x1b]52;c;aGk=\x07"
        );
        assert_eq!(
            crate::osc52_reply(
                ClipboardSelection::Primary,
                b"",
                ReplyTerminator::StringTerminator
            ),
            b"\x1b]52;p;\x1b\\"
        );
    }

    #[test]
    fn osc52_ignores_invalid_empty_and_unknown_sequences() {
        let mut terminal = terminal(8, 2);
        for sequence in [
            &b"\x1b]52;c;not base64!\x07"[..],
            b"\x1b]52;c;\x07",
            b"\x1b]52;c\x07",
            b"\x1b]52;0;aGk=\x07",
            b"\x1b]52;q;?\x07",
            b"\x1b]52;c;a\x07",
        ] {
            terminal.advance(sequence);
            assert_eq!(clipboard_events(&mut terminal), Vec::new(), "{sequence:?}");
        }
        terminal.advance(b"Z");
        assert_eq!(
            terminal.grid().row(0).unwrap()[0].content(),
            CellContent::Scalar('Z')
        );
    }

    #[test]
    fn osc52_payloads_are_bounded_beyond_the_ordinary_osc_limit() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        let mut terminal = terminal(8, 2);
        let largest = vec![b'x'; OSC52_MAX_BYTES];
        let mut sequence = b"\x1b]52;c;".to_vec();
        sequence.extend_from_slice(STANDARD.encode(&largest).as_bytes());
        sequence.push(0x07);
        assert!(sequence.len() > TerminalConfig::default().osc_limit);
        terminal.advance(&sequence);
        assert_eq!(
            clipboard_events(&mut terminal),
            vec![clipboard_set(ClipboardSelection::Clipboard, &largest)]
        );
        // One byte over still fits the string, and is refused decoded.
        let mut over = b"\x1b]52;c;".to_vec();
        over.extend_from_slice(STANDARD.encode(vec![b'x'; OSC52_MAX_BYTES + 1]).as_bytes());
        over.push(0x07);
        terminal.advance(&over);
        assert_eq!(clipboard_events(&mut terminal), Vec::new());
        // Far over, the string itself overflows and stays synchronized.
        let mut far = b"\x1b]52;c;".to_vec();
        far.extend_from_slice(STANDARD.encode(vec![b'x'; 2 * OSC52_MAX_BYTES]).as_bytes());
        far.extend_from_slice(b"\x07Z");
        terminal.advance(&far);
        let events = terminal.drain_events().collect::<Vec<_>>();
        assert_eq!(events, vec![TerminalEvent::StringTruncated("OSC")]);
        assert_eq!(
            terminal.grid().row(0).unwrap()[0].content(),
            CellContent::Scalar('Z')
        );
        // Every other OSC keeps the configured limit.
        let mut title = b"\x1b]2;".to_vec();
        title.extend(std::iter::repeat_n(b't', 8192));
        title.push(0x07);
        terminal.advance(&title);
        assert_eq!(
            terminal.drain_events().collect::<Vec<_>>(),
            vec![TerminalEvent::StringTruncated("OSC")]
        );
    }

    #[test]
    fn osc52_contents_stay_out_of_terminal_updates() {
        let mut terminal = terminal(8, 2);
        let base = terminal.revision();
        terminal.advance(b"A\x1b]52;c;aGk=\x07B\x1b]52;c;?\x07C");
        assert_eq!(clipboard_events(&mut terminal).len(), 2);
        let updates = terminal.updates_since(base).unwrap();
        assert!(updates.updates().all(|update| update.events().all(|event| {
            !matches!(
                event,
                TerminalEvent::ClipboardSet { .. } | TerminalEvent::ClipboardQuery { .. }
            )
        })));
    }

    #[test]
    fn a_checkpoint_mid_osc52_drops_the_rest_and_stays_synchronized() {
        let config = TerminalConfig {
            osc_limit: 16,
            ..TerminalConfig::default()
        };
        let mut terminal = Terminal::new(8, 2, config.clone());
        terminal.advance(b"\x1b]52;c;");
        terminal.advance(&[b'A'; 40]);
        let mut whole = terminal.clone();
        let checkpoint = terminal.encode_checkpoint().unwrap();
        let mut restored = Terminal::restore_checkpoint(&checkpoint, config).unwrap();
        restored.advance(b"QUJD\x07Z");
        let events = restored.drain_events().collect::<Vec<_>>();
        assert_eq!(events, vec![TerminalEvent::StringTruncated("OSC")]);
        assert_eq!(
            restored.grid().row(0).unwrap()[0].content(),
            CellContent::Scalar('Z')
        );
        // Without the checkpoint the same bytes set the clipboard.
        whole.advance(b"QUJD\x07");
        let mut expected = vec![0_u8; 30];
        expected.extend_from_slice(b"ABC");
        assert_eq!(
            clipboard_events(&mut whole),
            vec![clipboard_set(ClipboardSelection::Clipboard, &expected)]
        );
    }

    #[test]
    fn osc_title_palette_query_and_limits_are_observable() {
        let config = TerminalConfig {
            osc_limit: 12,
            ..TerminalConfig::default()
        };
        let mut terminal = Terminal::new(8, 2, config);
        terminal.advance(b"\x1b]2;demo\x07\x1b]4;1;#abc\x07\x1b]4;1;?\x07");
        assert_eq!(terminal.title(), "demo");
        assert_eq!(terminal.palette()[1], 0xaa_bb_cc);
        let events = terminal.drain_events().collect::<Vec<_>>();
        assert!(
            events.iter().any(
                |event| matches!(event, TerminalEvent::TitleChanged(title) if title == "demo")
            )
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, TerminalEvent::PtyWrite(_)))
        );

        terminal.advance(b"\x1b]2;this-is-too-long\x07Z");
        assert!(matches!(
            terminal.drain_events().next(),
            Some(TerminalEvent::StringTruncated("OSC"))
        ));
        assert_eq!(
            terminal.grid().row(0).unwrap()[0].content(),
            CellContent::Scalar('Z')
        );
    }

    #[test]
    fn overwriting_either_half_of_wide_cell_clears_the_other_half() {
        let mut leader = terminal(4, 1);
        leader.advance("界\rX".as_bytes());
        assert_eq!(
            leader.grid().row(0).unwrap()[0].content(),
            CellContent::Scalar('X')
        );
        assert_eq!(
            leader.grid().row(0).unwrap()[1].content(),
            CellContent::Empty
        );

        let mut continuation = terminal(4, 1);
        continuation.advance("界\x1b[2GX".as_bytes());
        assert_eq!(
            continuation.grid().row(0).unwrap()[0].content(),
            CellContent::Empty
        );
        assert_eq!(
            continuation.grid().row(0).unwrap()[1].content(),
            CellContent::Scalar('X')
        );
    }

    #[test]
    fn wide_character_at_margin_without_autowrap_is_clipped_safely() {
        let mut terminal = terminal(2, 1);
        terminal.advance("a\x1b[?7l界".as_bytes());
        assert_eq!(
            terminal.grid().row(0).unwrap()[1].content(),
            CellContent::Scalar('界')
        );
        assert!(terminal.grid().row(0).unwrap().has_valid_wide_cells());
    }

    #[test]
    fn combining_mark_walks_back_to_a_wide_leader() {
        let mut terminal = terminal(4, 1);
        terminal.advance("界\u{301}".as_bytes());
        assert!(matches!(
            terminal.grid().row(0).unwrap()[0].content(),
            CellContent::Composed(_)
        ));
        assert_eq!(
            terminal.grid().row(0).unwrap()[1].content(),
            CellContent::Spacer(1)
        );
    }

    #[test]
    fn editing_commands_clear_deferred_wrap() {
        for command in [b"\x1b[K".as_slice(), b"\x1b[X", b"\x1b[P", b"\x1b[@"] {
            let mut terminal = terminal(3, 2);
            terminal.advance(b"abc");
            assert!(terminal.grid().cursor().deferred_wrap());
            terminal.advance(command);
            assert!(!terminal.grid().cursor().deferred_wrap());
        }
    }

    #[test]
    fn private_cursor_query_and_ris_event_order_are_preserved() {
        let mut terminal = terminal(4, 2);
        terminal.advance(b"\x1b[?6n\x07\x1bc");
        assert_eq!(
            terminal.drain_events().collect::<Vec<_>>(),
            vec![
                TerminalEvent::PtyWrite(b"\x1b[?1;1R".to_vec()),
                TerminalEvent::Bell,
            ]
        );
    }

    #[test]
    fn event_queue_and_composed_sequences_are_bounded() {
        let config = TerminalConfig {
            event_limit: 2,
            ..TerminalConfig::default()
        };
        let mut terminal = Terminal::new(4, 1, config);
        terminal.advance(b"\x07\x07\x07");
        assert_eq!(terminal.drain_events().count(), 2);

        let mut input = String::from("a");
        input.extend(std::iter::repeat_n('\u{301}', 100));
        terminal.advance(input.as_bytes());
    }

    #[test]
    fn malformed_colors_and_excess_parameters_are_ignored() {
        let mut terminal = terminal(4, 1);
        terminal.advance(b"\x1b[1mA\x1b[38;2mB\x1b]4;1;rgb:00000000/0/0\x07");
        assert_eq!(
            terminal.grid().row(0).unwrap()[1].attributes().foreground(),
            Color::default()
        );
        assert_ne!(terminal.palette()[1], 0);

        terminal.advance(b"\x1b[0;0;0;0;0;0;0;0;0;0;0;0;0;0;0;0;31mC");
        assert_eq!(
            terminal.grid().row(0).unwrap()[2].attributes().foreground(),
            Color::default()
        );
    }

    #[test]
    fn osc_controls_are_ignored_and_escape_dispatches_before_next_sequence() {
        let mut terminal = terminal(4, 2);
        terminal.advance(b"\x1b]2;de\x01mo\x1b[2;2HZ");
        assert_eq!(terminal.title(), "demo");
        assert_eq!(terminal.grid().cursor().position(), Coordinate::new(2, 1));
    }

    #[test]
    fn default_color_osc_set_query_and_reset_are_bounded() {
        let mut terminal = terminal(4, 1);
        let initial = terminal.default_colors()[0];
        terminal.advance(b"\x1b]10;#123456\x07\x1b]10;?\x07\x1b]110\x1b\\");
        assert_eq!(terminal.default_colors()[0], initial);
        assert!(terminal.drain_events().any(|event| {
            matches!(event, TerminalEvent::PtyWrite(bytes) if String::from_utf8_lossy(&bytes).contains("rgb:1212/3434/5656"))
        }));
    }

    #[test]
    fn sgr_21_preserves_bold_and_palette_queries_use_16_bit_components() {
        let mut terminal = terminal(4, 1);
        terminal.advance(b"\x1b[1;21mA\x1b]4;1;?\x07");
        let attributes = terminal.grid().row(0).unwrap()[0].attributes();
        assert!(attributes.bold());
        assert_eq!(attributes.underline_style(), UnderlineStyle::Double);
        let replies = terminal.drain_events().collect::<Vec<_>>();
        assert!(replies.iter().any(|event| {
            matches!(event, TerminalEvent::PtyWrite(bytes) if String::from_utf8_lossy(bytes).contains("rgb:8080/0000/0000"))
        }));
    }

    #[test]
    fn row_damage_detects_underline_style_and_color_only_changes() {
        let mut original = crate::Row::new(1);
        original[0]
            .attributes_mut()
            .set_underline_style(UnderlineStyle::Single);

        let mut styled = original.clone();
        styled[0]
            .attributes_mut()
            .set_underline_style(UnderlineStyle::Curly);
        assert!(!rows_semantically_equal(&original, &styled));

        let mut colored = original.clone();
        colored[0]
            .attributes_mut()
            .set_underline_color(Color::rgb(0x12_34_56));
        assert!(!rows_semantically_equal(&original, &colored));
    }

    #[test]
    fn bounded_search_covers_history_visible_unicode_case_and_cursor_pages() {
        let mut terminal = Terminal::new(
            12,
            2,
            TerminalConfig {
                scrollback_lines: 8,
                ..TerminalConfig::default()
            },
        );
        terminal.advance("Alpha\r\nbeta 界\r\nalpha\r\ngamma".as_bytes());
        let first = terminal.search_normal("ALPHA", false, 0, 1, Duration::from_secs(1));
        assert_eq!(first.matches.len(), 1);
        assert!(first.has_older);
        assert_eq!(first.matches[0].preview.trim(), "alpha");
        let second = terminal.search_normal(
            "ALPHA",
            false,
            first.next_offset.unwrap(),
            4,
            Duration::from_secs(1),
        );
        assert_eq!(second.matches.len(), 1);
        assert_ne!(first.matches[0].row_id, second.matches[0].row_id);
        let wide = terminal.search_normal("界", true, 0, 4, Duration::from_secs(1));
        assert_eq!(wide.matches.len(), 1);
        assert!(wide.matches[0].end_column > wide.matches[0].start_column);
        assert_eq!(first.history_generation, second.history_generation);
    }

    #[test]
    fn a_row_cursor_continues_across_appends_and_timeouts_lose_no_row() {
        let mut terminal = Terminal::new(
            20,
            2,
            TerminalConfig {
                scrollback_lines: 16,
                ..TerminalConfig::default()
            },
        );
        // All four in history, below two blank screen rows.
        terminal.advance(b"hit 1\r\nhit 2\r\nhit 3\r\nhit 4\r\n\r\n");
        let first = terminal.search_normal("hit", false, 0, 1, Duration::from_secs(1));
        assert_eq!(first.matches[0].preview, "hit 4");
        assert!(!first.revision_bound);
        let cursor = first.next_before_row_id.expect("older rows remain");

        terminal.advance(b"hit 5\r\nhit 6\r\n");
        let next = terminal.search_normal(
            "hit",
            false,
            crate::SearchFrom::BeforeRow(cursor),
            8,
            Duration::from_secs(1),
        );
        let previews: Vec<_> = next
            .matches
            .iter()
            .map(|item| item.preview.as_str())
            .collect();
        assert_eq!(previews, ["hit 3", "hit 2", "hit 1"]);
        assert!(!next.has_older);
        assert_eq!(next.next_before_row_id, None);
        assert!(!next.revision_bound);

        // "hit 6" is on the screen now: that match holds only at this revision.
        let newest = terminal.search_normal("hit", false, 0, 1, Duration::from_secs(1));
        assert_eq!(newest.matches[0].preview, "hit 6");
        assert!(newest.revision_bound);

        // A zero deadline still reads one row per page and resumes at the
        // row it stopped on.
        let mut found = Vec::new();
        let mut from = crate::SearchFrom::Offset(0);
        for _ in 0..32 {
            let page = terminal.search_normal("hit", false, from, 8, Duration::ZERO);
            found.extend(page.matches.into_iter().map(|item| item.preview));
            from = match (page.next_before_row_id, page.next_offset) {
                (Some(before), _) => crate::SearchFrom::BeforeRow(before),
                (None, Some(offset)) => crate::SearchFrom::Offset(offset),
                (None, None) => break,
            };
        }
        assert_eq!(
            found,
            ["hit 6", "hit 5", "hit 4", "hit 3", "hit 2", "hit 1"]
        );
    }

    #[test]
    fn search_stops_at_written_cells_but_keeps_blanks_a_query_ends_with() {
        let mut terminal = Terminal::new(
            263,
            2,
            TerminalConfig {
                scrollback_lines: 8,
                ..TerminalConfig::default()
            },
        );
        terminal.advance("tail\r\n\r\nMiDdLe é\r\nend".as_bytes());
        let search =
            |query: &str| terminal.search_normal(query, false, 0, 8, Duration::from_secs(1));

        let spaced = search("tail  ");
        assert_eq!(spaced.matches.len(), 1);
        assert_eq!(
            (spaced.matches[0].start_column, spaced.matches[0].end_column),
            (0, 6)
        );
        assert_eq!(spaced.matches[0].preview, "tail");

        let mixed = search("middle É");
        assert_eq!(mixed.matches.len(), 1);
        assert_eq!(
            (mixed.matches[0].start_column, mixed.matches[0].end_column),
            (0, 8)
        );
        assert_eq!(mixed.matches[0].preview, "MiDdLe é");

        // Every row, the blank one included, holds a single blank.
        assert_eq!(search(" ").matches.len(), 4);
        assert!(search("tail   x").matches.is_empty());
    }

    #[test]
    fn oversized_tab_counts_stop_at_the_edges_promptly() {
        let mut terminal = Terminal::new(20, 2, TerminalConfig::default());
        let started = std::time::Instant::now();
        terminal.advance(b"\x1b[2147483647I");
        let column = |terminal: &Terminal| {
            terminal
                .snapshot(SnapshotRequest::default())
                .cursor()
                .cursor
                .position()
                .column
        };
        assert_eq!(column(&terminal), 19);
        terminal.advance(b"\x1b[2147483647Z");
        assert_eq!(column(&terminal), 0);
        terminal.advance(b"\x1b[2I");
        assert_eq!(column(&terminal), 16);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn synchronized_update_modes_accept_dec_and_cava_boundaries() {
        let mut terminal = Terminal::new(8, 2, TerminalConfig::default());
        assert!(!terminal.synchronized_updates());

        terminal.advance(b"\x1b[?2026h");
        assert!(terminal.synchronized_updates());
        terminal.advance(b"A");
        terminal.advance(b"\x1b[?2026l");
        assert!(!terminal.synchronized_updates());

        terminal.advance(b"\x1b[2026l\x1b\\B\x1b[2026h\x1b\\C");
        assert!(!terminal.synchronized_updates());
        assert_eq!(
            terminal.grid().row(0).unwrap()[0].content(),
            CellContent::Scalar('A')
        );
        assert_eq!(
            terminal.grid().row(0).unwrap()[1].content(),
            CellContent::Scalar('B')
        );
        assert_eq!(
            terminal.grid().row(0).unwrap()[2].content(),
            CellContent::Scalar('C')
        );

        terminal.advance(b"\x1b[2026l");
        assert!(terminal.synchronized_updates());
        terminal.advance(b"\x1bc");
        assert!(!terminal.synchronized_updates());
    }

    #[test]
    fn large_synchronized_redraw_commits_one_ordered_update() {
        const LINES: usize = 2_000;
        let mut terminal = Terminal::new(16, 4, TerminalConfig::default());
        let base_revision = terminal.revision();
        let mut frame = Vec::with_capacity(LINES * 6 + 20);
        frame.extend_from_slice(b"\x1b[?2026h");
        for _ in 0..LINES {
            frame.extend_from_slice(b"line\r\n");
        }
        frame.extend_from_slice(b"\x1b[?2026l");

        terminal.advance(&frame);

        let updates = terminal
            .updates_since(base_revision)
            .expect("large synchronized redraw update");
        assert_eq!(updates.updates().len(), 1);
        let update = updates.updates().next().expect("synchronized update");
        assert!(update.damage().count() > LINES);
        assert!(
            update
                .damage()
                .any(|damage| matches!(damage, TerminalDamage::Scroll { .. }))
        );
        assert!(
            update
                .damage()
                .all(|damage| !matches!(damage, TerminalDamage::FullSnapshot))
        );
        assert!((0..4).any(|row| row_text(&terminal, row).contains("line")));
    }

    #[test]
    fn synchronized_boundary_advance_stops_before_the_next_cava_frame() {
        let mut terminal = Terminal::new(8, 2, TerminalConfig::default());
        let base_revision = terminal.revision();
        let bytes = b"\x1b[2026lA\x1b[2026h\x1b\\\x1b[2026lB";
        let (consumed, completed) = terminal.advance_to_synchronized_boundary(bytes);

        assert!(completed);
        assert_eq!(&bytes[..consumed], b"\x1b[2026lA\x1b[2026h");
        assert!(!terminal.synchronized_updates());
        let updates = terminal
            .updates_since(base_revision)
            .expect("Cava update batch");
        assert_eq!(updates.updates().len(), 1);
        let damage = updates
            .updates()
            .next()
            .expect("Cava update")
            .damage()
            .collect::<Vec<_>>();
        assert!(
            damage
                .iter()
                .any(|damage| matches!(damage, TerminalDamage::Rows { .. }))
        );
        assert!(
            damage
                .iter()
                .all(|damage| !matches!(damage, TerminalDamage::FullSnapshot))
        );
        assert_eq!(
            terminal.grid().row(0).unwrap()[0].content(),
            CellContent::Scalar('A')
        );
        assert_eq!(
            terminal.grid().row(0).unwrap()[1].content(),
            CellContent::Empty
        );

        terminal.advance(&bytes[consumed..]);
        assert!(terminal.synchronized_updates());
        assert_eq!(
            terminal.grid().row(0).unwrap()[1].content(),
            CellContent::Scalar('B')
        );
    }

    #[test]
    fn unsupported_dcs_and_malformed_bytes_recover_without_panicking() {
        let mut terminal = terminal(8, 2);
        terminal.advance(b"\x1bPxpayload\x1b\\A\xf0(\x8c(B");
        assert!(
            terminal
                .drain_events()
                .any(|event| event == TerminalEvent::UnsupportedSequence("DCS"))
        );
        assert!(row_text(&terminal, 0).contains('A'));

        let mut state = 0x1234_5678_u64;
        for _ in 0..2_000 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            terminal.advance(&[state.to_le_bytes()[3]]);
            let cursor = terminal.grid().cursor().position();
            assert!(cursor.column >= 0 && usize::try_from(cursor.column).unwrap() < 8);
            assert!(cursor.row >= 0 && usize::try_from(cursor.row).unwrap() < 2);
        }
    }
}
