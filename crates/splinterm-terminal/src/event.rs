//! Observable semantic effects produced while parsing terminal input.
//!
//! Phase 4 will add revisions, snapshots, and renderer damage. These effects
//! exist now because bell, replies, title changes, and bounded-string failures
//! are part of Phase 3 terminal semantics.

use base64::{Engine as _, engine::general_purpose::STANDARD};

/// The most bytes one OSC 52 sequence may set, once decoded (Plan 0074 M2).
pub const OSC52_MAX_BYTES: usize = 1024 * 1024;

/// The longest base64 text that decodes to at most [`OSC52_MAX_BYTES`].
pub const OSC52_MAX_BASE64_BYTES: usize = OSC52_MAX_BYTES.div_ceil(3) * 4;

/// A selection an OSC 52 sequence names (Plan 0074 M2).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ClipboardSelection {
    /// `c`, and an empty target: the clipboard.
    Clipboard,
    /// `p`, and `s`, as foot and Alacritty read it: the primary selection.
    Primary,
}

impl ClipboardSelection {
    /// The target letter an OSC 52 reply names.
    #[must_use]
    pub const fn letter(self) -> u8 {
        match self {
            Self::Clipboard => b'c',
            Self::Primary => b'p',
        }
    }
}

/// How an OSC query ended; its reply ends the same way.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ReplyTerminator {
    /// BEL.
    Bell,
    /// `ESC \` or C1 ST.
    StringTerminator,
}

impl ReplyTerminator {
    /// The terminator's bytes.
    #[must_use]
    pub const fn bytes(self) -> &'static [u8] {
        match self {
            Self::Bell => b"\x07",
            Self::StringTerminator => b"\x1b\\",
        }
    }
}

/// The reply to an OSC 52 query: `ESC ] 52 ; <target> ; <base64> <ST>`,
/// ending as the query did. Empty `bytes` give the empty reply, which a
/// program reads as "nothing to paste" rather than waiting.
#[must_use]
pub fn osc52_reply(
    selection: ClipboardSelection,
    bytes: &[u8],
    terminator: ReplyTerminator,
) -> Vec<u8> {
    let encoded = STANDARD.encode(bytes);
    let mut reply = Vec::with_capacity(encoded.len() + 9);
    reply.extend_from_slice(b"\x1b]52;");
    reply.push(selection.letter());
    reply.push(b';');
    reply.extend_from_slice(encoded.as_bytes());
    reply.extend_from_slice(terminator.bytes());
    reply
}

/// A terminal effect in parser order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalEvent {
    /// BEL was executed.
    Bell,
    /// Bytes must be written back to the PTY.
    PtyWrite(Vec<u8>),
    /// OSC changed the terminal title.
    TitleChanged(String),
    /// OSC changed or reset one palette entry.
    PaletteChanged {
        /// Palette index.
        index: u16,
        /// Packed `0xRRGGBB` value.
        color: u32,
    },
    /// A recognized family has no semantic handler in this milestone.
    UnsupportedSequence(&'static str),
    /// A terminal image command was rejected without committing partial state.
    ImageRejected(&'static str),
    /// A string exceeded its configured retention limit but remained synced.
    StringTruncated(&'static str),
    /// The bounded one-shot event queue dropped at least one event.
    EventQueueOverflow,
    /// OSC 52 asked to set `selection` to `bytes`: decoded from base64,
    /// never empty, and at most [`OSC52_MAX_BYTES`]. The terminal touches
    /// no clipboard; its owner decides whether and where to publish them.
    /// Kept out of terminal updates, so only the drain sees them.
    ClipboardSet {
        selection: ClipboardSelection,
        bytes: Vec<u8>,
    },
    /// OSC 52 asked for `selection`'s contents. The terminal never answers
    /// itself: its owner writes [`osc52_reply`], empty when it has nothing
    /// to give, so the program never waits for an answer that never comes.
    ClipboardQuery {
        selection: ClipboardSelection,
        terminator: ReplyTerminator,
    },
}
