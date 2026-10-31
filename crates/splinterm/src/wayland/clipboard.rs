//! Bounded clipboard policy, worker admission, and deadline-driven pipe I/O.

use std::{
    borrow::Cow,
    io,
    os::fd::{AsFd, OwnedFd},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::Sender as StdSender,
    },
    task::Waker,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use rustix::event::{PollFd, PollFlags, Timespec, poll};

use smithay_client_toolkit::seat::keyboard::{Keysym, Modifiers};
use splinterm_protocol::TerminalInputModes;

use super::{
    DragOffer,
    file_drop::{FileDropTarget, MAX_DROP_BYTES},
    key_input,
};

pub(super) const TEXT_MIMES: [&str; 3] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"];
pub(super) const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;
pub(super) const BRACKETED_PASTE_OVERHEAD: usize = 12;
const MAX_CLIPBOARD_WORKERS: usize = 4;
pub(super) const CLIPBOARD_IO_TIMEOUT: Duration = Duration::from_secs(2);
/// Plan 0074: how long a paste waits for the program that owns the clipboard
/// or selection to send its text. Longer than [`CLIPBOARD_IO_TIMEOUT`], since
/// a busy or remote source can take a moment to answer.
pub(super) const PASTE_READ_TIMEOUT: Duration = Duration::from_secs(5);
/// The most terminal input one paste may be once encoded, bracketing
/// included: the daemon's per-Splint input queue holds 1 MiB, and a paste
/// larger than that could only arrive cut short.
pub(super) const MAX_PASTE_INPUT_BYTES: usize = MAX_CLIPBOARD_BYTES;
pub(super) static ACTIVE_CLIPBOARD_WORKERS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OwnedFieldTarget {
    BindingHelp,
    CommandPalette,
    DojoPrompt,
    ExplorerSearch,
    Search,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PasteTarget {
    Clipboard,
    Primary,
    OwnedField(OwnedFieldTarget),
    FileDrop(FileDropTarget),
}

pub(super) struct ClipboardRead {
    pub(super) target: PasteTarget,
    pub(super) input_generation: u64,
    pub(super) drag_offer: Option<DragOffer>,
    pub(super) bytes: io::Result<Vec<u8>>,
}

pub(super) struct ClipboardWorkerPermit<'a> {
    active: &'a AtomicUsize,
}

impl Drop for ClipboardWorkerPermit<'_> {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Encodes clipboard bytes for the terminal's current bracketed-paste mode.
///
/// With bracketed paste on, the bytes are wrapped unchanged. With it off,
/// each line end (`\r\n` or a lone `\n`) is sent as `\r`, the byte the Enter
/// key sends, as xterm, foot and Alacritty do (Plan 0074).
#[must_use]
pub fn encode_bracketed_paste(bytes: &[u8], enabled: bool) -> Vec<u8> {
    if !enabled {
        let mut encoded = Vec::with_capacity(bytes.len());
        let mut previous = None;
        for &byte in bytes {
            match byte {
                b'\n' if previous == Some(b'\r') => {}
                b'\n' => encoded.push(b'\r'),
                _ => encoded.push(byte),
            }
            previous = Some(byte);
        }
        return encoded;
    }
    let mut encoded = Vec::with_capacity(bytes.len() + BRACKETED_PASTE_OVERHEAD);
    encoded.extend_from_slice(b"\x1b[200~");
    encoded.extend_from_slice(bytes);
    encoded.extend_from_slice(b"\x1b[201~");
    encoded
}

pub(super) fn accepted_text_mime(mimes: &[String]) -> Option<String> {
    TEXT_MIMES.iter().find_map(|supported| {
        mimes
            .iter()
            .find(|mime| mime.as_str() == *supported)
            .cloned()
    })
}

/// Plan 0074: why a paste did not reach its target. Each shows a short
/// Window notice, so a paste never vanishes without a word.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PasteDrop {
    Timeout,
    Busy,
    TooLarge,
    NotUtf8,
    ReadFailed,
    NoText,
    FocusChanged,
    InputPaused,
    NoInput,
    Backlog,
}

impl PasteDrop {
    /// The Window notice: short, and saying why.
    pub(super) const fn notice(self) -> &'static str {
        match self {
            Self::Timeout => "Paste dropped: the clipboard did not answer in time",
            Self::Busy => "Paste dropped: too many clipboard transfers at once",
            Self::TooLarge => "Paste dropped: more than 1 MiB of text",
            Self::NotUtf8 => "Paste dropped: the text is not UTF-8",
            Self::ReadFailed => "Paste dropped: the clipboard could not be read",
            Self::NoText => "Nothing pasted: the clipboard holds no text",
            Self::FocusChanged => "Paste dropped: focus changed while it was read",
            Self::InputPaused => "Paste dropped: input is paused",
            Self::NoInput => "Paste dropped: this pane takes no input",
            Self::Backlog => "Paste dropped: earlier input is still being sent",
        }
    }

    /// Why a clipboard read failed, from its error.
    pub(super) fn from_read_error(error: &io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::TimedOut => Self::Timeout,
            io::ErrorKind::WouldBlock => Self::Busy,
            io::ErrorKind::InvalidData => Self::TooLarge,
            _ => Self::ReadFailed,
        }
    }
}

/// Plan 0074: why a finished terminal paste read can no longer be
/// delivered: the input target changed while it was read, or terminal
/// input is blocked now. `None` when it can.
pub(super) const fn clipboard_read_drop(
    input_blocked: bool,
    current_generation: u64,
    read_generation: u64,
) -> Option<PasteDrop> {
    if current_generation != read_generation {
        Some(PasteDrop::FocusChanged)
    } else if input_blocked {
        Some(PasteDrop::InputPaused)
    } else {
        None
    }
}

/// What a paste does with an offer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum PasteOffer {
    /// Read the text in this MIME type.
    Text(String),
    /// Plan 0074 (operator decision): a clipboard holding an image and no
    /// text sends the program `Ctrl+V`, so an agent such as Claude Code can
    /// read the image itself.
    ImageKey,
    NoText,
}

/// What pasting from an offer of `mimes` into `target` does. Only a
/// clipboard paste into the terminal passes an image on as `Ctrl+V`.
pub(super) fn paste_offer(mimes: &[String], target: PasteTarget) -> PasteOffer {
    if let Some(mime) = accepted_text_mime(mimes) {
        PasteOffer::Text(mime)
    } else if target == PasteTarget::Clipboard
        && mimes.iter().any(|mime| mime.starts_with("image/"))
    {
        PasteOffer::ImageKey
    } else {
        PasteOffer::NoText
    }
}

/// The bytes `Ctrl+V` sends in `modes`, as a typed `Ctrl+V` would.
pub(super) fn image_paste_key(modes: TerminalInputModes) -> Vec<u8> {
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::default()
    };
    key_input(Keysym::v, Some("v"), ctrl, modes).unwrap_or_else(|| vec![0x16])
}

/// Plan 0074: the text of a paste, with control characters other than tab,
/// line feed and carriage return removed rather than refusing the paste.
/// Removing ESC (and the C1 controls) means pasted text cannot end a
/// bracketed paste early or send an escape sequence of its own. Text over
/// 1 MiB or not UTF-8 is still refused.
pub(super) fn sanitize_paste(bytes: &[u8]) -> Result<Cow<'_, str>, PasteDrop> {
    if bytes.len() > MAX_CLIPBOARD_BYTES {
        return Err(PasteDrop::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| PasteDrop::NotUtf8)?;
    Ok(if text.chars().all(kept_in_paste) {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(
            text.chars()
                .filter(|character| kept_in_paste(*character))
                .collect(),
        )
    })
}

fn kept_in_paste(character: char) -> bool {
    !character.is_control() || matches!(character, '\t' | '\n' | '\r')
}

/// File drops stay strict: a URI list with control characters is refused.
pub(super) fn safe_paste(bytes: &[u8]) -> Result<&[u8]> {
    if bytes.len() > MAX_CLIPBOARD_BYTES {
        anyhow::bail!("clipboard offer exceeds the 1 MiB limit");
    }
    std::str::from_utf8(bytes).context("clipboard text is not UTF-8")?;
    if bytes
        .iter()
        .any(|byte| matches!(*byte, 0..=8 | 11..=12 | 14..=31 | 127))
    {
        anyhow::bail!("clipboard text contains unsafe control characters");
    }
    Ok(bytes)
}

pub(super) fn try_clipboard_worker(active: &AtomicUsize) -> Option<ClipboardWorkerPermit<'_>> {
    let mut current = active.load(Ordering::Acquire);
    loop {
        if current >= MAX_CLIPBOARD_WORKERS {
            return None;
        }
        match active.compare_exchange_weak(
            current,
            current + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return Some(ClipboardWorkerPermit { active }),
            Err(observed) => current = observed,
        }
    }
}

fn poll_timeout(deadline: Instant) -> Option<Timespec> {
    let remaining = deadline.checked_duration_since(Instant::now())?;
    Some(Timespec {
        tv_sec: i64::try_from(remaining.as_secs()).unwrap_or(i64::MAX),
        tv_nsec: i64::from(remaining.subsec_nanos()),
    })
}

fn wait_for_fd(fd: &impl AsFd, events: PollFlags, deadline: Instant) -> io::Result<bool> {
    let Some(timeout) = poll_timeout(deadline) else {
        return Ok(false);
    };
    let mut descriptor = [PollFd::new(fd, events)];
    let ready = poll(&mut descriptor, Some(&timeout)).map_err(io::Error::from)?;
    if ready == 0 {
        return Ok(false);
    }
    let returned = descriptor[0].revents();
    if returned.intersects(PollFlags::ERR | PollFlags::NVAL) {
        return Err(io::Error::other("clipboard pipe reported an I/O error"));
    }
    Ok(returned.intersects(events | PollFlags::HUP))
}

pub(super) fn read_clipboard_with_deadline(
    fd: &OwnedFd,
    timeout: Duration,
    maximum_bytes: usize,
) -> io::Result<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        if !wait_for_fd(fd, PollFlags::IN, deadline)? {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "clipboard read timed out",
            ));
        }
        let remaining = maximum_bytes.saturating_add(1).saturating_sub(bytes.len());
        if remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "clipboard offer exceeds byte limit",
            ));
        }
        let chunk_len = remaining.min(chunk.len());
        let read = rustix::io::read(fd, &mut chunk[..chunk_len]).map_err(io::Error::from)?;
        if read == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > maximum_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "clipboard offer exceeds byte limit",
            ));
        }
    }
}

pub(super) fn write_clipboard_with_deadline(
    fd: &OwnedFd,
    payload: &[u8],
    timeout: Duration,
) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    let mut written = 0;
    while written < payload.len() {
        if !wait_for_fd(fd, PollFlags::OUT, deadline)? {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "clipboard write timed out",
            ));
        }
        let end = (written + 4096).min(payload.len());
        let count = rustix::io::write(fd, &payload[written..end]).map_err(io::Error::from)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "clipboard pipe accepted no bytes",
            ));
        }
        written += count;
    }
    Ok(())
}

pub(super) fn spawn_clipboard_read(
    fd: OwnedFd,
    target: PasteTarget,
    input_generation: u64,
    drag_offer: Option<DragOffer>,
    tx: StdSender<ClipboardRead>,
    waker: Waker,
) {
    let Some(permit) = try_clipboard_worker(&ACTIVE_CLIPBOARD_WORKERS) else {
        let _ = tx.send(ClipboardRead {
            target,
            input_generation,
            drag_offer,
            bytes: Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "clipboard worker limit reached",
            )),
        });
        waker.wake();
        return;
    };
    std::thread::spawn(move || {
        let _permit = permit;
        let maximum_bytes = match target {
            PasteTarget::FileDrop(_) => MAX_DROP_BYTES,
            PasteTarget::Clipboard | PasteTarget::Primary | PasteTarget::OwnedField(_) => {
                MAX_CLIPBOARD_BYTES
            }
        };
        let bytes = read_clipboard_with_deadline(&fd, PASTE_READ_TIMEOUT, maximum_bytes);
        let _ = tx.send(ClipboardRead {
            target,
            input_generation,
            drag_offer,
            bytes,
        });
        waker.wake();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_policy_filters_mime_size_utf8_and_controls() {
        assert_eq!(
            accepted_text_mime(&[
                "image/png".to_owned(),
                "text/plain".to_owned(),
                "text/plain;charset=utf-8".to_owned(),
            ]),
            Some("text/plain;charset=utf-8".to_owned())
        );
        assert_eq!(accepted_text_mime(&["image/png".to_owned()]), None);
        assert_eq!(
            safe_paste(b"line one\nline two\t").expect("safe text"),
            b"line one\nline two\t"
        );
        assert!(safe_paste(b"unsafe\x1bsequence").is_err());
        assert!(safe_paste(&[0xff]).is_err());
        assert!(safe_paste(&vec![b'x'; MAX_CLIPBOARD_BYTES + 1]).is_err());
    }

    #[test]
    fn bracketed_paste_wraps_only_when_mode_is_enabled() {
        assert_eq!(encode_bracketed_paste(b"hello", false), b"hello");
        assert_eq!(
            encode_bracketed_paste(b"hello", true),
            b"\x1b[200~hello\x1b[201~"
        );
    }

    /// Plan 0074: an unbracketed paste sends each line end as the `\r`
    /// Enter sends; a bracketed one is left as copied.
    #[test]
    fn unbracketed_pastes_send_line_ends_as_carriage_returns() {
        assert_eq!(
            encode_bracketed_paste(b"one\ntwo\r\nthree\rfour\n\n", false),
            b"one\rtwo\rthree\rfour\r\r"
        );
        assert_eq!(
            encode_bracketed_paste(b"one\ntwo\r\n", true),
            b"\x1b[200~one\ntwo\r\n\x1b[201~"
        );
    }

    /// Plan 0074: control characters are removed, not a reason to refuse
    /// the paste; tab and line ends stay. Too large or not UTF-8 is still
    /// refused, with a reason for the Window notice.
    #[test]
    fn pastes_lose_control_characters_and_keep_tab_and_line_ends() {
        assert_eq!(
            sanitize_paste(b"line one\nline two\t\r\n").unwrap(),
            Cow::Borrowed("line one\nline two\t\r\n")
        );
        assert!(matches!(
            sanitize_paste(b"plain").unwrap(),
            Cow::Borrowed("plain")
        ));
        // ESC, BEL, BS, FF, DEL, NUL and the C1 CSI are removed, so pasted
        // text cannot close a bracketed paste early.
        assert_eq!(
            sanitize_paste("a\x1b[201~b\x07c\x08d\x0ce\x7ff\0g\u{9b}h".as_bytes()).unwrap(),
            "a[201~bcdefgh"
        );
        assert_eq!(sanitize_paste("é\t界".as_bytes()).unwrap(), "é\t界");
        assert_eq!(sanitize_paste(&[0xff]), Err(PasteDrop::NotUtf8));
        assert_eq!(
            sanitize_paste(&vec![b'x'; MAX_CLIPBOARD_BYTES + 1]),
            Err(PasteDrop::TooLarge)
        );
        assert!(sanitize_paste(&vec![b'x'; MAX_CLIPBOARD_BYTES]).is_ok());
    }

    #[test]
    fn every_dropped_paste_has_a_short_notice() {
        let drops = [
            PasteDrop::Timeout,
            PasteDrop::Busy,
            PasteDrop::TooLarge,
            PasteDrop::NotUtf8,
            PasteDrop::ReadFailed,
            PasteDrop::NoText,
            PasteDrop::FocusChanged,
            PasteDrop::InputPaused,
            PasteDrop::NoInput,
            PasteDrop::Backlog,
        ];
        for dropped in drops {
            let notice = dropped.notice();
            assert!(!notice.is_empty() && notice.len() <= 60, "{notice}");
        }
        for (kind, expected) in [
            (io::ErrorKind::TimedOut, PasteDrop::Timeout),
            (io::ErrorKind::WouldBlock, PasteDrop::Busy),
            (io::ErrorKind::InvalidData, PasteDrop::TooLarge),
            (io::ErrorKind::BrokenPipe, PasteDrop::ReadFailed),
        ] {
            let error = io::Error::new(kind, "read");
            assert_eq!(PasteDrop::from_read_error(&error), expected);
        }
        assert_eq!(clipboard_read_drop(false, 3, 3), None);
        assert_eq!(
            clipboard_read_drop(true, 3, 3),
            Some(PasteDrop::InputPaused)
        );
        assert_eq!(
            clipboard_read_drop(true, 4, 3),
            Some(PasteDrop::FocusChanged)
        );
    }

    /// Plan 0074 (operator decision): a clipboard paste of an image with no
    /// text sends `Ctrl+V`, the byte a typed `Ctrl+V` sends; text wins when
    /// both are offered, and fields and middle clicks never send the key.
    #[test]
    fn an_image_only_clipboard_paste_sends_ctrl_v() {
        let image = ["image/png".to_owned()];
        let mixed = ["image/png".to_owned(), "text/plain".to_owned()];
        assert_eq!(
            paste_offer(&image, PasteTarget::Clipboard),
            PasteOffer::ImageKey
        );
        assert_eq!(
            paste_offer(&["image/jpeg".to_owned()], PasteTarget::Clipboard),
            PasteOffer::ImageKey
        );
        assert_eq!(
            paste_offer(&mixed, PasteTarget::Clipboard),
            PasteOffer::Text("text/plain".to_owned())
        );
        assert_eq!(
            paste_offer(&image, PasteTarget::Primary),
            PasteOffer::NoText
        );
        assert_eq!(
            paste_offer(&image, PasteTarget::OwnedField(OwnedFieldTarget::Search)),
            PasteOffer::NoText
        );
        assert_eq!(
            paste_offer(&["application/x-thing".to_owned()], PasteTarget::Clipboard),
            PasteOffer::NoText
        );
        let modes = TerminalInputModes {
            application_cursor: false,
            application_keypad: false,
            focus_reporting: false,
            bracketed_paste: true,
            cursor_visible: true,
            cursor_blink: true,
            mouse_tracking: splinterm_protocol::MouseTracking::None,
            sgr_mouse: false,
        };
        assert_eq!(image_paste_key(modes), vec![0x16]);
    }

    #[test]
    fn clipboard_worker_budget_is_strict_and_released() {
        let active = AtomicUsize::new(0);
        let permits: Vec<_> = (0..MAX_CLIPBOARD_WORKERS)
            .map(|_| try_clipboard_worker(&active).expect("worker slot"))
            .collect();
        assert!(try_clipboard_worker(&active).is_none());
        drop(permits);
        assert_eq!(active.load(Ordering::Acquire), 0);
        assert!(try_clipboard_worker(&active).is_some());
    }

    #[test]
    fn clipboard_worker_budget_remains_strict_under_contention() {
        let active = AtomicUsize::new(0);
        let barrier = std::sync::Barrier::new(9);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    barrier.wait();
                    let permit = try_clipboard_worker(&active);
                    barrier.wait();
                    barrier.wait();
                    drop(permit);
                });
            }
            barrier.wait();
            barrier.wait();
            let admitted = active.load(Ordering::Acquire);
            barrier.wait();
            assert_eq!(admitted, MAX_CLIPBOARD_WORKERS);
        });
        assert_eq!(active.load(Ordering::Acquire), 0);
        assert!(try_clipboard_worker(&AtomicUsize::new(usize::MAX)).is_none());
    }

    #[test]
    fn clipboard_read_deadline_expires_without_a_writer() {
        use std::os::unix::net::UnixStream;

        let (reader, _writer) = UnixStream::pair().expect("socket pair");
        let fd = OwnedFd::from(reader);
        let error =
            read_clipboard_with_deadline(&fd, Duration::from_millis(5), MAX_CLIPBOARD_BYTES)
                .expect_err("idle peer times out");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}
