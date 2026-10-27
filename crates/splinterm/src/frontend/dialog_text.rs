//! The text of a trusted dialog, kept as logical lines so the Window can lay
//! it out again for whatever size the compositor gives it.
//!
//! A dialog is drawn as one box inside its Window (Plan 0065): a `warning`
//! frame through the middle of the grid's first and last rows and columns,
//! the title set into the top edge, the keyboard legend into the bottom one,
//! and two labelled buttons, Deny on the left and Allow on the right, above
//! the legend. Between them sit the body rows, inset
//! [`FRAME_INSET_COLUMNS`] from each side. The frame and the button fills
//! are painted by the Window (`wayland::chrome`); every character, the
//! title, the button labels and the legend among them, is a cell of the
//! dialog's static snapshot, in the terminal grid font.
//!
//! A dialog asks for one fixed size, its *natural* size: as wide as its
//! longest body row plus the frame (within
//! [`NATURAL_MIN_COLUMNS`]..=[`NATURAL_MAX_COLUMNS`]) and as its button
//! labels need, and tall enough for its body and the [`CHROME_ROWS`].
//! Tiling compositors float a Window whose minimum and maximum sizes are
//! equal. A compositor that gives it another size anyway gets the same text
//! wrapped again, at spaces, to the columns that fit (see [`layout`]).

use serde::{Deserialize, Serialize};
use splinterm_core::SplintId;
use splinterm_protocol::{
    ActiveScreen, CellAttributes, ColorSource, TerminalCell, TerminalInputModes, TerminalRow,
    TerminalSnapshot, UnderlineStyle,
};

/// The narrowest a dialog asks to be, in columns, frame included.
pub const NATURAL_MIN_COLUMNS: usize = 64;

/// The widest a dialog's body rows are laid out, in columns. A longer line
/// wraps.
pub const NATURAL_MAX_TEXT_COLUMNS: usize = 120;

/// The columns the frame takes on each side of the body: the frame's own
/// column and one blank column inside it.
pub const FRAME_INSET_COLUMNS: usize = 2;

/// The widest a dialog asks to be, in columns, frame included.
pub const NATURAL_MAX_COLUMNS: usize = NATURAL_MAX_TEXT_COLUMNS + 2 * FRAME_INSET_COLUMNS;

/// The grid rows above the body: the title row, which the frame's top edge
/// runs through, and one blank row.
pub const CHROME_TOP_ROWS: usize = 2;

/// The grid rows below the body: a blank row, the button labels, the keys
/// under them, a blank row, and the legend row, which the frame's bottom
/// edge runs through.
pub const CHROME_BOTTOM_ROWS: usize = 5;

/// Every grid row that is not body.
pub const CHROME_ROWS: usize = CHROME_TOP_ROWS + CHROME_BOTTOM_ROWS;

/// The columns a button label keeps clear of the dialog's middle, where the
/// two buttons meet.
const BUTTON_MIDDLE_COLUMNS: usize = 1;

/// What shows in place of the last body row kept when a dialog too small
/// even at its smallest font must drop rows.
pub const CLIPPED_MARKER: &str = "…";

/// The mark before every trusted dialog's title.
pub const TITLE_MARK: &str = "⚠";

/// The least columns between the title and the countdown in the frame's
/// top edge, so a stretch of frame shows between them.
pub const COUNTDOWN_GAP_COLUMNS: usize = 3;

/// What a line is: body text the layout wraps and may drop, or one of the
/// texts the box sets into its frame and buttons.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DialogLineRole {
    #[default]
    Body,
    /// How to answer: dropped last when rows must go.
    Hint,
    /// The lowercase title set into the frame's top edge.
    Title,
    /// The red left (no) button's label.
    Deny,
    /// The green right (yes) button's label: what it grants or does.
    Allow,
}

impl DialogLineRole {
    /// Whether the line is body text, laid out between the frame's edges.
    #[must_use]
    pub const fn is_body(self) -> bool {
        matches!(self, Self::Body | Self::Hint)
    }
}

/// How a body line is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DialogStyle {
    /// Primary text.
    #[default]
    Text,
    /// Primary text in bold: the request itself.
    Strong,
    /// Secondary text: notes, labels, choices that are not offered.
    Secondary,
    /// The selected choice: accent text on the selection fill across the
    /// box.
    Selected,
    /// A group heading in secondary text with a rule to the box's right.
    Group,
}

/// One logical line of dialog text: a paragraph the layout wraps at spaces.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogLine {
    /// The text. Leading spaces indent its first row.
    pub text: String,
    /// The columns every wrapped row after the first is indented by.
    pub indent: usize,
    /// The widest this line is laid out, in columns, even in a wider
    /// dialog; `None` for the dialog's width.
    pub wrap_at: Option<usize>,
    pub role: DialogLineRole,
    #[serde(default)]
    pub style: DialogStyle,
    /// How many characters at the start of its first row are a label, drawn
    /// in secondary text (a key in a key/value table).
    #[serde(default)]
    pub lead: usize,
    /// The access consent duration a click on any of its rows selects.
    #[serde(skip)]
    pub duration: Option<usize>,
    /// Lines in one slot take as many rows in every variant (selection) of
    /// a dialog as in its tallest, so choosing never moves the rows below.
    /// An empty line in a slot takes no row; it only marks where the slot
    /// is.
    #[serde(skip)]
    pub slot: Option<u8>,
}

impl DialogLine {
    /// A body line.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    /// An empty line.
    #[must_use]
    pub fn blank() -> Self {
        Self::default()
    }

    /// A line that says how to answer, dropped last when rows must go.
    #[must_use]
    pub fn hint(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            role: DialogLineRole::Hint,
            ..Self::default()
        }
    }

    /// The title set into the frame's top edge.
    #[must_use]
    pub fn title(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            role: DialogLineRole::Title,
            ..Self::default()
        }
    }

    /// The red left button's label.
    #[must_use]
    pub fn deny(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            role: DialogLineRole::Deny,
            ..Self::default()
        }
    }

    /// The green right button's label.
    #[must_use]
    pub fn allow(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            role: DialogLineRole::Allow,
            ..Self::default()
        }
    }

    /// Draws it in `style`.
    #[must_use]
    pub const fn with_style(mut self, style: DialogStyle) -> Self {
        self.style = style;
        self
    }

    /// Draws the first `characters` of its first row as a label.
    #[must_use]
    pub const fn with_lead(mut self, characters: usize) -> Self {
        self.lead = characters;
        self
    }

    /// Indents its wrapped rows by `columns`.
    #[must_use]
    pub const fn with_indent(mut self, columns: usize) -> Self {
        self.indent = columns;
        self
    }

    /// Wraps it at `columns` at most, however wide the dialog is.
    #[must_use]
    pub const fn with_wrap_at(mut self, columns: usize) -> Self {
        self.wrap_at = Some(columns);
        self
    }

    /// Marks every row of it as selecting the duration at `index`.
    #[must_use]
    pub const fn with_duration(mut self, index: usize) -> Self {
        self.duration = Some(index);
        self
    }

    /// Puts it in `slot` (see [`DialogLine::slot`]).
    #[must_use]
    pub const fn in_slot(mut self, slot: u8) -> Self {
        self.slot = Some(slot);
        self
    }
}

/// A dialog's whole text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DialogText {
    pub lines: Vec<DialogLine>,
    /// The fewest grid rows the dialog asks for.
    pub minimum_rows: usize,
}

impl DialogText {
    /// The text of its first line with `role`, or "" without one.
    fn chrome_text(&self, role: DialogLineRole) -> &str {
        self.lines
            .iter()
            .find(|line| line.role == role)
            .map_or("", |line| line.text.as_str())
    }

    /// The title set into the frame.
    #[must_use]
    pub fn title(&self) -> &str {
        self.chrome_text(DialogLineRole::Title)
    }

    /// The red left button's label.
    #[must_use]
    pub fn deny_label(&self) -> &str {
        self.chrome_text(DialogLineRole::Deny)
    }

    /// The green right button's label.
    #[must_use]
    pub fn allow_label(&self) -> &str {
        self.chrome_text(DialogLineRole::Allow)
    }

    /// Whether both button labels fit their buttons in a dialog `columns`
    /// wide. A label that does not fit is cut, and a dialog that cuts its
    /// Allow label cannot say yes.
    #[must_use]
    pub fn buttons_fit(&self, columns: usize) -> bool {
        let room = button_label_columns(columns);
        [self.deny_label(), self.allow_label()]
            .iter()
            .all(|label| label.chars().count() <= room)
    }
}

/// The columns each button's label may take in a dialog `columns` wide:
/// its half of the box, inside the frame and clear of the middle.
#[must_use]
pub const fn button_label_columns(columns: usize) -> usize {
    (columns / 2).saturating_sub(FRAME_INSET_COLUMNS + BUTTON_MIDDLE_COLUMNS)
}

/// The columns body rows may take in a dialog `columns` wide.
#[must_use]
pub const fn text_columns(columns: usize) -> usize {
    let inside = columns.saturating_sub(2 * FRAME_INSET_COLUMNS);
    if inside == 0 { 1 } else { inside }
}

/// Where the box's rows go in a grid `grid_rows` tall. A grid shorter than
/// [`CHROME_ROWS`] has no room for body rows, and its rows overlap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChromeRows {
    /// The row of the button labels.
    pub label: usize,
    /// The row of the keys under them.
    pub keys: usize,
    /// The row the frame's bottom edge runs through, with the legend.
    pub legend: usize,
    /// How many body rows fit between the title and the buttons.
    pub body_room: usize,
}

/// The box's rows in a grid `grid_rows` tall: the buttons and the legend
/// keep the bottom, the body takes what is left below the title.
#[must_use]
pub const fn chrome_rows(grid_rows: usize) -> ChromeRows {
    ChromeRows {
        label: grid_rows.saturating_sub(4),
        keys: grid_rows.saturating_sub(3),
        legend: grid_rows.saturating_sub(1),
        body_room: grid_rows.saturating_sub(CHROME_ROWS),
    }
}

/// The columns, from first to past the last, a button's label may take:
/// the left (deny) half or the right (allow) half of a box `columns` wide.
#[must_use]
pub const fn button_columns(columns: usize, allow: bool) -> (usize, usize) {
    let room = button_label_columns(columns);
    if allow {
        let end = columns.saturating_sub(FRAME_INSET_COLUMNS);
        (end.saturating_sub(room), end)
    } else {
        (FRAME_INSET_COLUMNS, FRAME_INSET_COLUMNS + room)
    }
}

/// What a dialog's snapshot is called and which Splint identity it carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogIdentity {
    pub title: String,
    pub splint_id: SplintId,
    pub incarnation: u64,
}

impl Default for DialogIdentity {
    fn default() -> Self {
        Self {
            title: String::new(),
            splint_id: SplintId::new(),
            incarnation: 0,
        }
    }
}

/// `line` wrapped into rows of at most `columns` characters, or of its own
/// [`DialogLine::wrap_at`] when that is narrower. It breaks at spaces and
/// keeps the spaces between words on one row; only a word wider than a whole
/// row is split. An empty line is one empty row.
#[must_use]
pub fn wrap_line(line: &DialogLine, columns: usize) -> Vec<String> {
    let width = line
        .wrap_at
        .map_or(columns, |wrap| wrap.min(columns))
        .max(1);
    // A hanging indent never takes more than half a row.
    let indent = line.indent.min(width / 2);
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut row_chars = 0;
    let mut row_has_word = false;
    let mut gap = 0;
    let mut word = Vec::new();
    let mut words = Vec::new();
    for character in line.text.chars() {
        if character == ' ' {
            if !word.is_empty() {
                words.push((gap, std::mem::take(&mut word)));
                gap = 0;
            }
            gap += 1;
        } else {
            word.push(character);
        }
    }
    if !word.is_empty() {
        words.push((gap, word));
    }
    for (gap, mut word) in words {
        loop {
            // The first row keeps its leading spaces; a wrapped row starts
            // at the indent instead.
            let mut separator = if row_has_word || rows.is_empty() {
                gap
            } else {
                0
            };
            if row_chars + separator + word.len() <= width {
                row.extend(std::iter::repeat_n(' ', separator));
                row.extend(word.iter());
                row_chars += separator + word.len();
                row_has_word = true;
                break;
            }
            if row_has_word {
                rows.push(std::mem::take(&mut row));
                row.extend(std::iter::repeat_n(' ', indent));
                row_chars = indent;
                row_has_word = false;
                continue;
            }
            // A word that fits a row once its leading spaces go keeps them
            // out; only a word wider than any row is split.
            if row_chars + word.len() <= width {
                row.extend(word.iter());
                row_chars += word.len();
                row_has_word = true;
                break;
            }
            if row_chars + separator >= width {
                separator = 0;
            }
            let room = width.saturating_sub(row_chars + separator).max(1);
            row.extend(std::iter::repeat_n(' ', separator));
            row.extend(word.drain(..room.min(word.len())));
            rows.push(std::mem::take(&mut row));
            row.extend(std::iter::repeat_n(' ', indent));
            row_chars = indent;
            row_has_word = false;
            if word.is_empty() {
                break;
            }
        }
    }
    if row_has_word || rows.is_empty() {
        if !row_has_word {
            row.clear();
        }
        rows.push(row);
    }
    rows
}

/// How one laid-out body row is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowStyle {
    pub style: DialogStyle,
    /// The characters at its start drawn as a label.
    pub lead: usize,
}

/// One wrapped row and where it came from.
struct LaidRow {
    text: String,
    role: DialogLineRole,
    duration: Option<usize>,
    style: RowStyle,
}

/// How many rows each slot takes in `text` at `columns`.
fn slot_rows(text: &DialogText, columns: usize) -> Vec<(u8, usize)> {
    let mut slots: Vec<(u8, usize)> = Vec::new();
    for line in text.lines.iter().filter(|line| line.role.is_body()) {
        let Some(slot) = line.slot else { continue };
        let rows = if line.text.trim().is_empty() {
            0
        } else {
            wrap_line(line, columns).len()
        };
        match slots.iter_mut().find(|(id, _)| *id == slot) {
            Some((_, total)) => *total += rows,
            None => slots.push((slot, rows)),
        }
    }
    slots
}

/// Every slot's tallest row count over `variants` at `columns`.
fn tallest_slots(variants: &[DialogText], columns: usize) -> Vec<(u8, usize)> {
    let mut tallest: Vec<(u8, usize)> = Vec::new();
    for variant in variants {
        for (slot, rows) in slot_rows(variant, columns) {
            match tallest.iter_mut().find(|(id, _)| *id == slot) {
                Some((_, most)) => *most = (*most).max(rows),
                None => tallest.push((slot, rows)),
            }
        }
    }
    tallest
}

/// `text` wrapped at `columns`, each slot padded to `tallest`.
fn wrapped_rows(text: &DialogText, columns: usize, tallest: &[(u8, usize)]) -> Vec<LaidRow> {
    let mut rows = Vec::new();
    let mut used: Vec<(u8, usize)> = Vec::new();
    for (index, line) in text.lines.iter().enumerate() {
        if !line.role.is_body() {
            continue;
        }
        let anchor_only = line.slot.is_some() && line.text.trim().is_empty();
        if !anchor_only {
            for (wrapped, row) in wrap_line(line, columns).into_iter().enumerate() {
                rows.push(LaidRow {
                    text: row,
                    role: line.role,
                    duration: line.duration,
                    style: RowStyle {
                        style: line.style,
                        // The label is on the first row only.
                        lead: if wrapped == 0 { line.lead } else { 0 },
                    },
                });
            }
        }
        let Some(slot) = line.slot else { continue };
        let taken = if anchor_only {
            0
        } else {
            wrap_line(line, columns).len()
        };
        match used.iter_mut().find(|(id, _)| *id == slot) {
            Some((_, total)) => *total += taken,
            None => used.push((slot, taken)),
        }
        // After the slot's last line, blank rows fill it to its tallest.
        let last_of_slot = !text.lines[index + 1..]
            .iter()
            .any(|later| later.slot == Some(slot));
        if last_of_slot {
            let most = tallest
                .iter()
                .find_map(|(id, rows)| (*id == slot).then_some(*rows))
                .unwrap_or(0);
            let have = used
                .iter()
                .find_map(|(id, rows)| (*id == slot).then_some(*rows))
                .unwrap_or(0);
            for _ in have..most {
                rows.push(LaidRow {
                    text: String::new(),
                    role: line.role,
                    duration: None,
                    style: RowStyle::default(),
                });
            }
        }
    }
    rows
}

/// How many rows of text the tallest of `variants` takes at `columns`.
#[must_use]
pub fn text_rows(variants: &[DialogText], columns: usize) -> usize {
    let tallest = tallest_slots(variants, columns);
    variants
        .iter()
        .map(|variant| wrapped_rows(variant, columns, &tallest).len())
        .max()
        .unwrap_or(0)
}

/// The columns a dialog asks for, frame included: its widest body row, laid
/// out no wider than [`NATURAL_MAX_TEXT_COLUMNS`], and enough for its
/// title and button labels, within
/// [`NATURAL_MIN_COLUMNS`]..=[`NATURAL_MAX_COLUMNS`].
#[must_use]
pub fn natural_columns(variants: &[DialogText]) -> usize {
    let tallest = tallest_slots(variants, NATURAL_MAX_TEXT_COLUMNS);
    let body = variants
        .iter()
        .flat_map(|variant| wrapped_rows(variant, NATURAL_MAX_TEXT_COLUMNS, &tallest))
        .map(|row| row.text.chars().count())
        .max()
        .unwrap_or(1);
    let chrome = variants
        .iter()
        .map(|variant| {
            let label = variant
                .deny_label()
                .chars()
                .count()
                .max(variant.allow_label().chars().count());
            // The smallest width whose buttons hold the label.
            let buttons = 2 * (label + FRAME_INSET_COLUMNS + BUTTON_MIDDLE_COLUMNS);
            let title = title_text(variant.title()).chars().count();
            buttons.max(title + 2 * FRAME_INSET_COLUMNS)
        })
        .max()
        .unwrap_or(0);
    (body + 2 * FRAME_INSET_COLUMNS)
        .max(chrome)
        .clamp(NATURAL_MIN_COLUMNS, NATURAL_MAX_COLUMNS)
}

/// The columns a dialog's body rows wrap at in its natural size.
#[must_use]
pub fn natural_text_columns(variants: &[DialogText]) -> usize {
    text_columns(natural_columns(variants))
}

/// The grid rows a dialog asks for at [`natural_columns`]: its body rows
/// and the [`CHROME_ROWS`], and at least its [`DialogText::minimum_rows`].
#[must_use]
pub fn natural_rows(variants: &[DialogText]) -> usize {
    let rows = text_rows(variants, natural_text_columns(variants));
    let minimum = variants
        .iter()
        .map(|variant| variant.minimum_rows)
        .max()
        .unwrap_or(0);
    (rows + CHROME_ROWS).max(minimum)
}

/// One laid-out rendering of a dialog's body text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DialogLayout {
    /// The rows of text, top to bottom.
    pub rows: Vec<String>,
    /// How each row is drawn, one per row.
    pub styles: Vec<RowStyle>,
    /// Every row that shows a duration, with that duration's index.
    pub duration_rows: Vec<(usize, usize)>,
    /// Whether rows were dropped because they did not fit.
    pub clipped: bool,
}

/// Lays out `variants[selected]` at `columns`, in at most `max_rows` rows.
///
/// Slots keep the rows of every variant in the same places. When the text
/// needs more than `max_rows` rows, the body is cut from its end and its last
/// kept row becomes [`CLIPPED_MARKER`], so every hint row stays; the hint
/// rows are cut from their end only when they alone do not fit. Rows keep
/// their order either way.
#[must_use]
pub fn layout(
    variants: &[DialogText],
    selected: usize,
    columns: usize,
    max_rows: usize,
) -> DialogLayout {
    layout_with_notice(variants, selected, columns, max_rows, None)
}

/// [`layout`], except that when the text does not fit, `notice` (when
/// given) takes the place of every hint row: a dialog that hides part of
/// its text says so instead of saying how to say yes.
#[must_use]
pub fn layout_with_notice(
    variants: &[DialogText],
    selected: usize,
    columns: usize,
    max_rows: usize,
    notice: Option<&str>,
) -> DialogLayout {
    layout_hiding(variants, selected, columns, max_rows, notice, false)
}

/// [`layout_with_notice`] for a dialog that also hides text outside its
/// body when `hidden` (a button label cut to fit): it counts as clipped,
/// and shows its notice, even when every body row fits.
#[must_use]
pub fn layout_hiding(
    variants: &[DialogText],
    selected: usize,
    columns: usize,
    max_rows: usize,
    notice: Option<&str>,
    hidden: bool,
) -> DialogLayout {
    let Some(text) = variants.get(selected).or_else(|| variants.first()) else {
        return DialogLayout::default();
    };
    let tallest = tallest_slots(variants, columns);
    let mut rows = wrapped_rows(text, columns, &tallest);
    let clipped = hidden || rows.len() > max_rows;
    if let Some(notice) = notice.filter(|_| clipped) {
        rows.retain(|row| row.role != DialogLineRole::Hint);
        rows.extend(
            wrap_line(&DialogLine::hint(notice), columns)
                .into_iter()
                .map(|text| LaidRow {
                    text,
                    role: DialogLineRole::Hint,
                    duration: None,
                    style: RowStyle::default(),
                }),
        );
    }
    let keep: Vec<bool> = if rows.len() > max_rows {
        let hints = rows
            .iter()
            .filter(|row| row.role == DialogLineRole::Hint)
            .count();
        let mut body_room = max_rows.saturating_sub(hints);
        let mut hint_room = max_rows.min(hints);
        rows.iter()
            .map(|row| {
                let room = if row.role == DialogLineRole::Hint {
                    &mut hint_room
                } else {
                    &mut body_room
                };
                let kept = *room > 0;
                *room = room.saturating_sub(1);
                kept
            })
            .collect()
    } else {
        vec![true; rows.len()]
    };
    // The marker shows only where body rows were dropped.
    let body_dropped = rows
        .iter()
        .zip(&keep)
        .any(|(row, kept)| !*kept && row.role == DialogLineRole::Body);
    let last_body = body_dropped
        .then(|| {
            rows.iter()
                .zip(&keep)
                .rposition(|(row, kept)| *kept && row.role == DialogLineRole::Body)
        })
        .flatten();
    let mut laid = DialogLayout {
        clipped,
        ..DialogLayout::default()
    };
    for (index, (row, kept)) in rows.into_iter().zip(keep).enumerate() {
        if !kept {
            continue;
        }
        if Some(index) == last_body {
            laid.rows.push(CLIPPED_MARKER.to_owned());
            laid.styles.push(RowStyle::default());
            continue;
        }
        if let Some(duration) = row.duration {
            laid.duration_rows.push((laid.rows.len(), duration));
        }
        laid.rows.push(row.text);
        laid.styles.push(row.style);
    }
    laid
}

/// `variants[selected]` laid out at its natural size, every row shown.
#[must_use]
pub fn natural_layout(variants: &[DialogText], selected: usize) -> DialogLayout {
    layout(
        variants,
        selected,
        natural_text_columns(variants),
        usize::MAX,
    )
}

/// The title as the frame shows it: the warning mark, then the title, or
/// nothing without one.
fn title_text(title: &str) -> String {
    if title.is_empty() {
        String::new()
    } else {
        format!("{TITLE_MARK} {title}")
    }
}

/// What a dialog cell is drawn in. Each ink but [`DialogInk::Text`] is one
/// slot of the snapshot's palette, which the Window fills from the dialog's
/// floored colors (`wayland::chrome::trusted_dialog_theme`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DialogInk {
    /// Primary text: the snapshot's default foreground.
    #[default]
    Text,
    /// Secondary text on the dialog's background.
    Secondary,
    /// The title, in warning text.
    Title,
    /// A selected choice's text, on the selection fill.
    SelectedText,
    /// The Deny label, on the red tint.
    DenyLabel,
    /// The Allow label, on the green tint.
    AllowLabel,
    /// The Deny keys, on the red tint.
    DenyKeys,
    /// The Allow keys, on the green tint.
    AllowKeys,
    /// The Allow label and keys while it cannot say yes.
    AllowOff,
}

impl DialogInk {
    /// Every ink that takes a palette slot.
    pub const SLOTTED: [Self; 8] = [
        Self::Secondary,
        Self::Title,
        Self::SelectedText,
        Self::DenyLabel,
        Self::AllowLabel,
        Self::DenyKeys,
        Self::AllowKeys,
        Self::AllowOff,
    ];

    /// The palette slot it is drawn from, or `None` for the default
    /// foreground.
    #[must_use]
    pub const fn slot(self) -> Option<usize> {
        match self {
            Self::Text => None,
            Self::Secondary => Some(1),
            Self::Title => Some(2),
            Self::SelectedText => Some(3),
            Self::DenyLabel => Some(4),
            Self::AllowLabel => Some(5),
            Self::DenyKeys => Some(6),
            Self::AllowKeys => Some(7),
            Self::AllowOff => Some(8),
        }
    }
}

/// The palette slot of the selection fill behind a selected choice.
pub const SELECTION_SLOT: usize = 9;

/// One cell of a composed dialog.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DialogCell {
    pub character: char,
    pub ink: DialogInk,
    pub bold: bool,
    /// Whether it sits on the selection fill.
    pub selected: bool,
}

/// What the box shows besides the body: the button keys and labels'
/// state, and the legend set into the frame's bottom edge.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DialogChrome {
    /// The keys under the Deny label.
    pub deny_keys: String,
    /// The keys under the Allow label.
    pub allow_keys: String,
    /// Whether the dialog can say yes now; the Allow label is drawn off
    /// while it cannot.
    pub allow_enabled: bool,
    /// The legend's forms, best first, each as runs of text and ink. The
    /// first that fits inside the frame is shown; none, when none fits.
    pub legend: Vec<Vec<(String, DialogInk)>>,
    /// The time left to answer, such as "0:42", set into the frame's top
    /// edge at its right end; dropped, never cut, when it would not fit
    /// beside the whole title.
    pub countdown: Option<String>,
}

/// A composed grid's rows, each a run of cells from column 0.
type ComposedRows = Vec<Vec<DialogCell>>;

/// Writes `text` into `row` from `column`, no further than `end`.
fn put(row: &mut Vec<DialogCell>, column: usize, end: usize, text: &str, cell: DialogCell) {
    for (offset, character) in text.chars().enumerate() {
        let at = column + offset;
        if at >= end {
            break;
        }
        if row.len() <= at {
            row.resize(
                at + 1,
                DialogCell {
                    character: ' ',
                    ..DialogCell::default()
                },
            );
        }
        row[at] = DialogCell { character, ..cell };
    }
}

/// `text` cut to `columns` characters, its last an ellipsis when it was
/// cut.
fn fitted(text: &str, columns: usize) -> String {
    if text.chars().count() <= columns {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(columns.saturating_sub(1)).collect();
    if columns > 0 {
        cut.push_str(CLIPPED_MARKER);
    }
    cut
}

/// `text` (a dialog's laid-out `body`) composed into the box of a grid
/// `columns` by `grid_rows`: the title on the frame's top edge (and the
/// countdown at its right end, while both fit), the body
/// below it, the buttons' labels and keys centred in their halves, and the
/// first form of the legend that fits on the frame's bottom edge.
///
/// The body is placed as laid out, never cut here: the layout already kept
/// it to [`ChromeRows::body_room`] rows of [`text_columns`]. A label wider
/// than its button is cut ([`DialogText::buttons_fit`] says when).
#[must_use]
pub fn compose(
    text: &DialogText,
    body: &DialogLayout,
    chrome: &DialogChrome,
    columns: usize,
    grid_rows: usize,
) -> ComposedRows {
    let mut rows: ComposedRows = vec![Vec::new(); grid_rows.max(1)];
    let inside_end = columns.saturating_sub(FRAME_INSET_COLUMNS);
    let places = chrome_rows(grid_rows);
    let title = title_text(text.title());
    // The countdown keeps a stretch of frame between it and the title, and
    // goes before the title would be cut.
    let countdown = chrome.countdown.as_deref().filter(|countdown| {
        title.chars().count() + COUNTDOWN_GAP_COLUMNS + countdown.chars().count()
            <= text_columns(columns)
    });
    put(
        &mut rows[0],
        FRAME_INSET_COLUMNS,
        inside_end,
        &fitted(&title, text_columns(columns)),
        DialogCell {
            ink: DialogInk::Title,
            bold: true,
            ..DialogCell::default()
        },
    );
    if let Some(countdown) = countdown {
        let width = countdown.chars().count();
        put(
            &mut rows[0],
            inside_end - width,
            inside_end,
            countdown,
            DialogCell {
                ink: DialogInk::Secondary,
                ..DialogCell::default()
            },
        );
    }
    for (index, (row_text, style)) in body.rows.iter().zip(&body.styles).enumerate() {
        let Some(row) = rows.get_mut(CHROME_TOP_ROWS + index) else {
            break;
        };
        compose_body_row(row, row_text, *style, columns);
    }
    for (allow, label) in [(false, text.deny_label()), (true, text.allow_label())] {
        let (start, end) = button_columns(columns, allow);
        let (label_ink, keys_ink) = match (allow, chrome.allow_enabled) {
            (false, _) => (DialogInk::DenyLabel, DialogInk::DenyKeys),
            (true, true) => (DialogInk::AllowLabel, DialogInk::AllowKeys),
            (true, false) => (DialogInk::AllowOff, DialogInk::AllowOff),
        };
        let keys = if allow {
            &chrome.allow_keys
        } else {
            &chrome.deny_keys
        };
        for (row, shown, ink, bold) in [
            (places.label, fitted(label, end - start), label_ink, true),
            (places.keys, fitted(keys, end - start), keys_ink, false),
        ] {
            let width = shown.chars().count();
            let column = start + (end - start - width) / 2;
            if let Some(row) = rows.get_mut(row) {
                put(
                    row,
                    column,
                    end,
                    &shown,
                    DialogCell {
                        ink,
                        bold,
                        ..DialogCell::default()
                    },
                );
            }
        }
    }
    let room = text_columns(columns);
    if let Some(form) = chrome.legend.iter().find(|form| {
        form.iter()
            .map(|(text, _)| text.chars().count())
            .sum::<usize>()
            <= room
    }) && let Some(row) = rows.get_mut(places.legend)
    {
        let mut column = FRAME_INSET_COLUMNS;
        for (text, ink) in form {
            put(
                row,
                column,
                inside_end,
                text,
                DialogCell {
                    ink: *ink,
                    ..DialogCell::default()
                },
            );
            column += text.chars().count();
        }
    }
    rows
}

/// One body row in its style, inset inside the frame.
fn compose_body_row(row: &mut Vec<DialogCell>, text: &str, style: RowStyle, columns: usize) {
    let inside_end = columns.saturating_sub(FRAME_INSET_COLUMNS);
    let selected = style.style == DialogStyle::Selected;
    if selected {
        // The selection fill runs across the box, inside the frame.
        let fill = DialogCell {
            character: ' ',
            selected: true,
            ..DialogCell::default()
        };
        row.clear();
        row.push(DialogCell {
            character: ' ',
            ..DialogCell::default()
        });
        row.resize(columns.saturating_sub(1).max(1), fill);
    }
    let (ink, bold) = match style.style {
        DialogStyle::Text => (DialogInk::Text, false),
        DialogStyle::Strong => (DialogInk::Text, true),
        DialogStyle::Secondary | DialogStyle::Group => (DialogInk::Secondary, false),
        DialogStyle::Selected => (DialogInk::SelectedText, false),
    };
    let cell = DialogCell {
        ink,
        bold,
        selected,
        ..DialogCell::default()
    };
    let lead: String = text.chars().take(style.lead).collect();
    let rest: String = text.chars().skip(style.lead).collect();
    let lead_ink = if selected {
        DialogInk::SelectedText
    } else {
        DialogInk::Secondary
    };
    put(
        row,
        FRAME_INSET_COLUMNS,
        inside_end,
        &lead,
        DialogCell {
            ink: lead_ink,
            ..cell
        },
    );
    let after_lead = FRAME_INSET_COLUMNS + lead.chars().count();
    put(row, after_lead, inside_end, &rest, cell);
    if style.style == DialogStyle::Group {
        let start = FRAME_INSET_COLUMNS + text.chars().count() + 1;
        let rule = "─".repeat(inside_end.saturating_sub(start));
        put(row, start, inside_end, &rule, cell);
    }
}

fn dialog_input_modes() -> TerminalInputModes {
    TerminalInputModes {
        application_cursor: false,
        application_keypad: false,
        focus_reporting: false,
        bracketed_paste: false,
        cursor_visible: false,
        cursor_blink: false,
        mouse_tracking: splinterm_protocol::MouseTracking::None,
        sgr_mouse: false,
    }
}

/// The snapshot attributes of one composed cell: its ink and fill as
/// palette slots, and bold.
fn cell_attributes(cell: DialogCell) -> CellAttributes {
    let (foreground_source, foreground) =
        cell.ink.slot().map_or((ColorSource::Default, 0), |slot| {
            (ColorSource::Base16, u32::try_from(slot).unwrap_or(0))
        });
    let (background_source, background) = if cell.selected {
        (
            ColorSource::Base16,
            u32::try_from(SELECTION_SLOT).unwrap_or(0),
        )
    } else {
        (ColorSource::Default, 0)
    };
    CellAttributes {
        bold: cell.bold,
        dim: false,
        italic: false,
        underline: UnderlineStyle::None,
        underline_color_source: ColorSource::Default,
        underline_color: 0,
        strikethrough: false,
        blink: false,
        conceal: false,
        reverse: false,
        foreground_source,
        foreground,
        background_source,
        background,
    }
}

/// Plain rows of primary text as composed cells.
#[must_use]
pub fn plain_rows(rows: Vec<String>) -> Vec<Vec<DialogCell>> {
    rows.into_iter()
        .map(|row| {
            row.chars()
                .map(|character| DialogCell {
                    character,
                    ..DialogCell::default()
                })
                .collect()
        })
        .collect()
}

/// A static, cursorless snapshot of `rows` in a `columns` by `grid_rows`
/// grid, in the trusted dialog colors. The grid grows to hold every row;
/// blank rows fill the rest.
#[must_use]
pub fn dialog_snapshot(
    rows: Vec<Vec<DialogCell>>,
    columns: usize,
    grid_rows: usize,
    identity: &DialogIdentity,
) -> TerminalSnapshot {
    let columns = rows
        .iter()
        .map(Vec::len)
        .max()
        .unwrap_or(0)
        .max(columns)
        .max(1);
    let grid_rows = rows.len().max(grid_rows).max(1);
    let mut visible_rows: Vec<_> = rows
        .into_iter()
        .map(|row| TerminalRow {
            row_id: None,
            linebreak: false,
            cells: row
                .into_iter()
                .map(|cell| TerminalCell {
                    content: cell.character.to_string(),
                    spacer_remaining: None,
                    attributes: cell_attributes(cell),
                })
                .collect(),
        })
        .collect();
    visible_rows.resize_with(grid_rows, || TerminalRow {
        row_id: None,
        linebreak: false,
        cells: Vec::new(),
    });
    // A Window refuses a snapshot whose visible rows lack distinct, nonzero
    // identities (`TerminalSnapshot::validate`), before it maps.
    for (row, id) in visible_rows.iter_mut().zip(1_u64..) {
        row.row_id = Some(id);
    }
    TerminalSnapshot {
        splint_id: identity.splint_id,
        incarnation: identity.incarnation,
        revision: 1,
        columns,
        rows: grid_rows,
        cursor_column: -1,
        cursor_row: -1,
        cursor_deferred_wrap: false,
        active_screen: ActiveScreen::Normal,
        input_modes: dialog_input_modes(),
        palette: vec![0; 256],
        default_colors: [0x00f4_f0e8, 0x0014_1820, 0x00e0_a030],
        title: identity.title.clone(),
        visible_rows,
        history_generation: 1,
        oldest_available_scrollback_row_id: None,
        newest_available_scrollback_row_id: None,
        scrollback_rows: Vec::new(),
        available_scrollback_rows: 0,
        omitted_oldest_scrollback_rows: 0,
        images: None,
        exited_code: None,
        exited_signal: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: Vec<DialogLine>) -> Vec<DialogText> {
        vec![DialogText {
            lines,
            minimum_rows: 0,
        }]
    }

    #[test]
    fn rewrapping_keeps_words_whole_and_paragraphs_apart() {
        let line = DialogLine::new(
            "Proceeding closes these Windows. Their terminals keep running in the upgraded daemon.",
        );
        for columns in [20, 31, 47, 64] {
            let rows = wrap_line(&line, columns);
            for row in &rows {
                assert!(row.chars().count() <= columns, "{columns}: {row:?}");
                assert!(!row.starts_with(' ') && !row.ends_with(' '), "{row:?}");
            }
            // Joined back, every word is there, whole and in order.
            assert_eq!(rows.join(" "), line.text, "{columns}");
        }
        // Two paragraphs never run into one row.
        let laid = layout(
            &text(vec![
                DialogLine::new("first one"),
                DialogLine::new("second"),
            ]),
            0,
            40,
            usize::MAX,
        );
        assert_eq!(laid.rows, ["first one", "second"]);
    }

    #[test]
    fn wrapping_keeps_inner_spaces_indents_and_splits_only_overlong_words() {
        let key = DialogLine::hint("[D / Esc / Enter  Cancel]     [G  Proceed]");
        assert_eq!(wrap_line(&key, 80), [key.text.clone()]);
        assert_eq!(
            wrap_line(&key, 30),
            ["[D / Esc / Enter  Cancel]", "[G  Proceed]"]
        );
        let program = DialogLine::new("Program: /usr/bin/splinterm-mcp --stdio").with_indent(9);
        assert_eq!(
            wrap_line(&program, 31),
            ["Program: /usr/bin/splinterm-mcp", "         --stdio"]
        );
        // The leading spaces of the first row stay.
        let bullet = DialogLine::new("  • PID 5 — did not answer").with_indent(4);
        assert_eq!(
            wrap_line(&bullet, 16),
            ["  • PID 5 — did", "    not answer"]
        );
        // A path wider than a row is split, and only then.
        let path = DialogLine::new(format!("Path: /{}", "p".repeat(30))).with_indent(4);
        let rows = wrap_line(&path, 20);
        assert_eq!(rows[0], "Path:");
        assert!(rows.iter().all(|row| row.chars().count() <= 20), "{rows:?}");
        assert_eq!(
            rows.iter().map(|row| row.trim()).collect::<String>(),
            format!("Path:/{}", "p".repeat(30))
        );
        // Its own narrower width wins over the dialog's.
        let narrow = DialogLine::new("one two three").with_wrap_at(8);
        assert_eq!(wrap_line(&narrow, 100), ["one two", "three"]);
        assert_eq!(wrap_line(&DialogLine::blank(), 10), [""]);
        assert_eq!(wrap_line(&DialogLine::new("   "), 10), [""]);
    }

    #[test]
    fn natural_size_holds_the_widest_line_the_labels_and_the_chrome_rows() {
        let variants = vec![DialogText {
            lines: vec![
                DialogLine::title("a title"),
                DialogLine::new("TITLE"),
                DialogLine::new("x".repeat(80)),
                DialogLine::new("y ".repeat(90)).with_wrap_at(100),
                DialogLine::deny("Deny"),
                DialogLine::allow("Allow"),
            ],
            minimum_rows: 0,
        }];
        // The widest row is 50 `y`s wrapped at 100: 99 columns, and the
        // frame's inset on each side.
        assert_eq!(natural_columns(&variants), 99 + 2 * FRAME_INSET_COLUMNS);
        assert_eq!(natural_text_columns(&variants), 99);
        let rows = text_rows(&variants, 99);
        assert_eq!(rows, 4);
        assert_eq!(natural_rows(&variants), 4 + CHROME_ROWS);
        // Short text still asks for the narrowest width and its minimum rows.
        let short = vec![DialogText {
            lines: vec![DialogLine::new("hi")],
            minimum_rows: 18,
        }];
        assert_eq!(natural_columns(&short), NATURAL_MIN_COLUMNS);
        assert_eq!(natural_rows(&short), 18);
        // A long label or title widens the box until it fits.
        let labelled = |title: &str, allow: &str| {
            vec![DialogText {
                lines: vec![
                    DialogLine::title(title),
                    DialogLine::new("hi"),
                    DialogLine::deny("Deny"),
                    DialogLine::allow(allow),
                ],
                minimum_rows: 0,
            }]
        };
        let allow = "a".repeat(40);
        let wide = labelled("t", &allow);
        assert_eq!(natural_columns(&wide), 2 * (40 + 3));
        assert!(wide[0].buttons_fit(natural_columns(&wide)));
        assert!(!wide[0].buttons_fit(natural_columns(&wide) - 1));
        let titled = labelled(&"t".repeat(70), "Allow");
        assert_eq!(natural_columns(&titled), 70 + 2 + 2 * FRAME_INSET_COLUMNS);
        // Never wider than the cap, however long the text.
        let huge = text(vec![DialogLine::new("z".repeat(500))]);
        assert_eq!(natural_columns(&huge), NATURAL_MAX_COLUMNS);
        // Nothing is ever cut at the natural size, and the frame's texts
        // are not body rows.
        let laid = natural_layout(&variants, 0);
        assert!(!laid.clipped);
        assert_eq!(laid.rows.len(), rows);
        assert_eq!(laid.styles.len(), rows);
        assert!(
            !laid
                .rows
                .iter()
                .any(|row| row == "a title" || row == "Allow")
        );
    }

    #[test]
    fn slots_keep_every_variant_on_the_same_rows_at_any_width() {
        let variant = |grant: &str, shared: &str| DialogText {
            lines: vec![
                DialogLine::new("( ) one").with_duration(0),
                DialogLine::new("( ) two words here").with_duration(1),
                DialogLine::new(shared).in_slot(0),
                DialogLine::new(grant).in_slot(1),
                DialogLine::hint("hint"),
            ],
            minimum_rows: 0,
        };
        let variants = vec![
            variant("Grant: once", ""),
            variant(
                "Grant: always allow this for the workspace",
                "shared by any program",
            ),
        ];
        for columns in [12, 20, 40, 100] {
            let first = layout(&variants, 0, columns, usize::MAX);
            let second = layout(&variants, 1, columns, usize::MAX);
            assert_eq!(first.rows.len(), second.rows.len(), "{columns}");
            assert_eq!(first.rows.len(), text_rows(&variants, columns));
            assert_eq!(first.duration_rows, second.duration_rows);
            assert_eq!(first.rows.last(), Some(&"hint".to_owned()));
        }
        // Every row of a wrapped duration selects it.
        let laid = layout(&variants, 0, 12, usize::MAX);
        assert_eq!(laid.duration_rows, [(0, 0), (1, 1), (2, 1)]);
    }

    /// Clipping keeps the hint rows to the last: the body is cut first and
    /// says so, and only hints that alone do not fit are cut.
    #[test]
    fn clipping_drops_body_rows_before_the_hint_rows() {
        let variants = text(vec![
            DialogLine::new("TITLE"),
            DialogLine::new("body one"),
            DialogLine::new("body two"),
            DialogLine::new("body three").with_duration(0),
            DialogLine::hint("keys"),
            DialogLine::hint("click"),
            DialogLine::hint("Ctrl+Alt+G"),
        ]);
        let all = layout(&variants, 0, 40, 7);
        assert!(!all.clipped);
        assert_eq!(all.rows.len(), 7);
        assert_eq!(all.duration_rows, [(3, 0)]);
        let cut = layout(&variants, 0, 40, 5);
        assert!(cut.clipped);
        assert_eq!(
            cut.rows,
            ["TITLE", CLIPPED_MARKER, "keys", "click", "Ctrl+Alt+G"]
        );
        assert!(cut.duration_rows.is_empty());
        assert_eq!(
            layout(&variants, 0, 40, 3).rows,
            ["keys", "click", "Ctrl+Alt+G"]
        );
        assert_eq!(layout(&variants, 0, 40, 2).rows, ["keys", "click"]);
        assert!(layout(&variants, 0, 40, 0).rows.is_empty());
    }

    /// A dialog that hides text says so where its answer hints were.
    #[test]
    fn a_clipped_layout_shows_its_notice_instead_of_the_hints() {
        let variants = text(vec![
            DialogLine::new("TITLE"),
            DialogLine::new("body one"),
            DialogLine::new("body two"),
            DialogLine::hint("G grants"),
            DialogLine::hint("click green"),
        ]);
        let notice = Some("Part of this is hidden");
        let fits = layout_with_notice(&variants, 0, 40, 5, notice);
        assert!(!fits.clipped);
        assert_eq!(fits.rows[3..], ["G grants", "click green"]);
        let cut = layout_with_notice(&variants, 0, 40, 3, notice);
        assert!(cut.clipped);
        assert_eq!(
            cut.rows,
            ["TITLE", CLIPPED_MARKER, "Part of this is hidden"]
        );
        // Hiding only a hint still counts as hidden text, and the body,
        // all in view, gets no marker.
        let hints_only = layout_with_notice(&variants, 0, 40, 4, notice);
        assert!(hints_only.clipped);
        assert_eq!(
            hints_only.rows,
            ["TITLE", "body one", "body two", "Part of this is hidden"]
        );
        // The notice wraps like any line, and is cut last.
        let narrow = layout_with_notice(&variants, 0, 12, 3, notice);
        assert_eq!(narrow.rows, [CLIPPED_MARKER, "Part of this", "is hidden"]);
    }

    #[test]
    fn dialog_snapshots_validate_and_hold_every_row() {
        let identity = DialogIdentity {
            title: "Upgrade question".to_owned(),
            ..DialogIdentity::default()
        };
        let snapshot = dialog_snapshot(
            plain_rows(vec!["one".into(), "a longer row".into()]),
            4,
            6,
            &identity,
        );
        assert_eq!(snapshot.validate().map_err(|error| error.message), Ok(()));
        assert_eq!((snapshot.columns, snapshot.rows), (12, 6));
        assert_eq!(snapshot.title, "Upgrade question");
        assert_eq!(snapshot.cursor_row, -1);
    }

    /// The box's rows: the title on the first, the body from the third, the
    /// labels and keys above a blank row and the legend on the last.
    #[test]
    fn the_chrome_keeps_the_bottom_rows_and_the_buttons_keep_their_halves() {
        assert_eq!(
            chrome_rows(25),
            ChromeRows {
                label: 21,
                keys: 22,
                legend: 24,
                body_room: 18,
            }
        );
        assert_eq!(chrome_rows(3).body_room, 0);
        for columns in [20, 63, 64, 65, 100, 124] {
            let (deny_start, deny_end) = button_columns(columns, false);
            let (allow_start, allow_end) = button_columns(columns, true);
            assert_eq!(deny_end - deny_start, button_label_columns(columns));
            assert_eq!(allow_end - allow_start, button_label_columns(columns));
            // Inside the frame, and each wholly on its own side of the
            // middle, where the pointer's halves meet.
            assert_eq!(deny_start, FRAME_INSET_COLUMNS);
            assert_eq!(allow_end, columns - FRAME_INSET_COLUMNS);
            assert!(2 * deny_end < columns, "{columns}");
            assert!(2 * allow_start > columns, "{columns}");
            assert_eq!(text_columns(columns), columns - 2 * FRAME_INSET_COLUMNS);
        }
        assert_eq!(text_columns(3), 1);
    }

    fn row_string(row: &[DialogCell]) -> String {
        row.iter().map(|cell| cell.character).collect()
    }

    fn boxed() -> Vec<DialogText> {
        text(vec![
            DialogLine::title("agent access request"),
            DialogLine::new("'probe' wants to Look").with_style(DialogStyle::Strong),
            DialogLine::new("program   /usr/bin/probe").with_lead(10),
            DialogLine::new("how long").with_style(DialogStyle::Group),
            DialogLine::new("( ) Once").with_style(DialogStyle::Selected),
            DialogLine::hint("G or the green Allow button (right) grants."),
            DialogLine::deny("Deny"),
            DialogLine::allow("Allow Look once"),
        ])
    }

    fn legend(forms: &[&str]) -> Vec<Vec<(String, DialogInk)>> {
        forms
            .iter()
            .map(|form| vec![((*form).to_owned(), DialogInk::Secondary)])
            .collect()
    }

    #[test]
    fn compose_places_the_title_body_labels_keys_and_legend() {
        let variants = boxed();
        let (columns, grid_rows) = (64, 14);
        let body = layout(&variants, 0, text_columns(columns), 7);
        assert!(!body.clipped);
        let chrome = DialogChrome {
            deny_keys: "esc · d · ↵".to_owned(),
            allow_keys: "g".to_owned(),
            allow_enabled: true,
            legend: legend(&["l".repeat(61).as_str(), "click inside to use the keyboard"]),
            countdown: None,
        };
        let rows = compose(&variants[0], &body, &chrome, columns, grid_rows);
        assert_eq!(rows.len(), grid_rows);
        // The title, bold, in the frame's colour, from inside the frame.
        assert!(row_string(&rows[0]).starts_with("  ⚠ agent access request"));
        assert!(rows[0][2..].iter().take(3).all(|cell| cell.bold));
        assert_eq!(rows[0][2].ink, DialogInk::Title);
        assert!(rows[1].is_empty());
        // The body from the third row, inset, in its styles.
        assert_eq!(row_string(&rows[2]), "  'probe' wants to Look");
        assert!(rows[2][2].bold);
        assert_eq!(rows[3][2].ink, DialogInk::Secondary);
        assert_eq!(rows[3][12].ink, DialogInk::Text);
        let group = row_string(&rows[4]);
        assert!(group.starts_with("  how long ─"), "{group}");
        assert_eq!(group.chars().count(), columns - FRAME_INSET_COLUMNS);
        // The selected choice fills inside the frame, its text in accent.
        assert_eq!(rows[5].len(), columns - 1);
        assert!(!rows[5][0].selected);
        assert!(rows[5][1..].iter().all(|cell| cell.selected));
        assert_eq!(rows[5][2].ink, DialogInk::SelectedText);
        // The labels and keys centred in their halves, Deny on the left.
        let places = chrome_rows(grid_rows);
        let label_row = row_string(&rows[places.label]);
        let keys_row = row_string(&rows[places.keys]);
        for (allow, label, keys) in [
            (false, "Deny", "esc · d · ↵"),
            (true, "Allow Look once", "g"),
        ] {
            let (start, end) = button_columns(columns, allow);
            let mut at = label_row.chars().collect::<Vec<_>>();
            at.resize(columns, ' ');
            let found: String = at[start..end].iter().collect();
            assert_eq!(found.trim(), label, "{label_row:?}");
            let left = found.len() - found.trim_start().len();
            let right = found.len() - found.trim_end().len();
            assert!(left.abs_diff(right) <= 1, "{label}: {left} {right}");
            let key_cells: String = keys_row.chars().skip(start).take(end - start).collect();
            assert_eq!(key_cells.trim(), keys);
        }
        let deny_column = label_row.find('D').unwrap();
        assert_eq!(rows[places.label][deny_column].ink, DialogInk::DenyLabel);
        assert!(rows[places.label][deny_column].bold);
        let allow_column = label_row.chars().position(|c| c == 'A').unwrap();
        assert_eq!(rows[places.label][allow_column].ink, DialogInk::AllowLabel);
        let g = keys_row.chars().position(|c| c == 'g').unwrap();
        assert_eq!(rows[places.keys][g].ink, DialogInk::AllowKeys);
        assert!(!rows[places.keys][g].bold);
        // The first legend form that fits inside the frame: 61 columns do
        // not fit in 60.
        assert_eq!(
            row_string(&rows[places.legend]),
            "  click inside to use the keyboard"
        );
        assert!(rows[places.legend - 1].is_empty());
        // Nothing reaches the frame's columns.
        for row in &rows {
            let chars: Vec<char> = row.iter().map(|cell| cell.character).collect();
            for column in [0, 1, columns - 2, columns - 1] {
                assert!(chars.get(column).is_none_or(|c| *c == ' '), "{chars:?}");
            }
        }
    }

    #[test]
    fn compose_dims_a_disabled_allow_and_cuts_what_does_not_fit() {
        let variants = boxed();
        let chrome = DialogChrome {
            deny_keys: "esc · d · ↵".to_owned(),
            allow_keys: "g".to_owned(),
            allow_enabled: false,
            legend: legend(&["l".repeat(40).as_str()]),
            countdown: None,
        };
        // 30 columns: 12 per label, so "Allow Look once" is cut.
        let columns = 30;
        assert!(!variants[0].buttons_fit(columns));
        assert!(variants[0].buttons_fit(64));
        let body = layout_hiding(
            &variants,
            0,
            text_columns(columns),
            usize::MAX,
            Some("hidden"),
            !variants[0].buttons_fit(columns),
        );
        assert!(body.clipped);
        assert_eq!(body.rows.last().map(String::as_str), Some("hidden"));
        let rows = compose(&variants[0], &body, &chrome, columns, 40);
        let places = chrome_rows(40);
        let label_row = row_string(&rows[places.label]);
        assert!(label_row.contains("Allow Look …"), "{label_row:?}");
        let allow_column = label_row.chars().position(|c| c == 'A').unwrap();
        assert_eq!(rows[places.label][allow_column].ink, DialogInk::AllowOff);
        let g = row_string(&rows[places.keys])
            .chars()
            .position(|c| c == 'g')
            .unwrap();
        assert_eq!(rows[places.keys][g].ink, DialogInk::AllowOff);
        // No legend form fits, so none shows.
        assert!(rows[places.legend].is_empty());
        // Each ink but the default takes its own palette slot, apart from
        // the selection's.
        let mut slots: Vec<usize> = DialogInk::SLOTTED
            .iter()
            .filter_map(|ink| ink.slot())
            .collect();
        slots.push(SELECTION_SLOT);
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(slots.len(), DialogInk::SLOTTED.len() + 1);
        assert!(slots.iter().all(|slot| *slot < 16));
    }

    /// The countdown sits at the right end of the frame's top edge, apart
    /// from the title, and goes before the title would be cut: the title
    /// always shows whole when it fits alone.
    #[test]
    fn the_countdown_sits_at_the_frame_right_and_drops_before_the_title_is_cut() {
        let variants = text(vec![
            DialogLine::title("close windows to upgrade?"),
            DialogLine::new("body"),
        ]);
        let chrome = DialogChrome {
            countdown: Some("0:42".to_owned()),
            ..DialogChrome::default()
        };
        let title = "⚠ close windows to upgrade?";
        let title_columns = title.chars().count();
        let row0 = |columns: usize| {
            let body = layout(&variants, 0, text_columns(columns), usize::MAX);
            compose(&variants[0], &body, &chrome, columns, 10).swap_remove(0)
        };
        // Roomy: the title from the left, the countdown ending inside the
        // frame, in secondary text.
        let roomy = row0(64);
        let shown = row_string(&roomy);
        assert!(shown.starts_with(&format!("  {title}")), "{shown:?}");
        assert!(shown.ends_with("0:42"), "{shown:?}");
        assert_eq!(shown.chars().count(), 64 - FRAME_INSET_COLUMNS);
        assert_eq!(
            roomy[64 - FRAME_INSET_COLUMNS - 1].ink,
            DialogInk::Secondary
        );
        // Exactly room for both and the gap.
        let tight = title_columns + COUNTDOWN_GAP_COLUMNS + 4 + 2 * FRAME_INSET_COLUMNS;
        assert!(row_string(&row0(tight)).ends_with("0:42"));
        // One column less: the countdown goes, and the title stays whole.
        let narrower = row_string(&row0(tight - 1));
        assert!(!narrower.contains("0:42"), "{narrower:?}");
        assert_eq!(narrower.trim(), title);
        // Without one, nothing shows at the right.
        let none = DialogChrome::default();
        let body = layout(&variants, 0, text_columns(64), usize::MAX);
        let plain = row_string(&compose(&variants[0], &body, &none, 64, 10)[0]);
        assert_eq!(plain.trim(), title);
    }
}
