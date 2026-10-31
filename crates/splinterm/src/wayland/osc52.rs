//! The Window side of OSC 52 (Plan 0074 M2).
//!
//! The panes this Window controls hand it their programs' clipboard sets
//! and queries through its [`crate::osc52::Osc52Hub`]. Once per event-loop
//! turn the Window publishes each allowed set as its own selection, with
//! its newest input serial, and answers each query from its clipboard or
//! primary selection: text only, read off the event loop within a bound.
//! A selection this Window owns is answered from its own copy, with no
//! read at all. What `clipboard.osc52` refuses is answered empty.

use std::{os::fd::OwnedFd, sync::Arc};

use splinterm_protocol::ClipboardSelection;
use tokio::sync::oneshot;
use wayland_client::QueueHandle;

use crate::osc52::{Osc52Hub, Osc52Policy, Osc52Request, QUERY_READ_TIMEOUT, query_answer};

use super::{
    App,
    clipboard::{
        ACTIVE_CLIPBOARD_WORKERS, MAX_CLIPBOARD_BYTES, accepted_text_mime,
        read_clipboard_with_deadline, try_clipboard_worker,
    },
};

impl App {
    /// Serves the OSC 52 requests that arrived since the last turn.
    pub(super) fn apply_osc52_requests(&mut self, queue_handle: &QueueHandle<Self>) {
        let Some(requests) = self.clipboard.osc52.as_ref().map(Osc52Hub::drain) else {
            return;
        };
        let policy = self.clipboard.osc52_policy;
        for request in requests {
            match request {
                Osc52Request::Set { selection, text } => {
                    // A selection needs a serial of this Window's input; one
                    // that had none yet publishes nothing.
                    let Some(serial) = self.input.last_input_serial.filter(|_| policy.allows_set())
                    else {
                        continue;
                    };
                    let primary = matches!(selection, ClipboardSelection::Primary);
                    self.publish_clipboard_payload(queue_handle, serial, primary, text.as_bytes());
                }
                Osc52Request::Query { selection, answer } => {
                    if let Some(text) = self.own_selection_answer(selection, policy) {
                        let _ = answer.send(text);
                        continue;
                    }
                    match self
                        .osc52_read_pipe(selection)
                        .filter(|_| policy.allows_read())
                    {
                        Some(pipe) => spawn_osc52_read(pipe, policy, answer),
                        None => {
                            let _ = answer.send(Vec::new());
                        }
                    }
                }
            }
        }
    }

    /// The answer from the selection's text when this Window owns it, so a
    /// read needs no Wayland round trip and no clipboard worker.
    fn own_selection_answer(
        &self,
        selection: ClipboardSelection,
        policy: Osc52Policy,
    ) -> Option<Vec<u8>> {
        match selection {
            ClipboardSelection::Clipboard => own_answer(policy, &self.clipboard.clipboard_sources),
            ClipboardSelection::Primary => own_answer(policy, &self.clipboard.primary_sources),
        }
    }

    /// A pipe that receives the selection's text, if it offers text.
    fn osc52_read_pipe(&self, selection: ClipboardSelection) -> Option<OwnedFd> {
        match selection {
            ClipboardSelection::Clipboard => {
                let offer = self.clipboard.clipboard_offer.as_ref()?;
                let mime = offer.with_mime_types(accepted_text_mime)?;
                offer.receive(mime).ok().map(OwnedFd::from)
            }
            ClipboardSelection::Primary => {
                let offer = self.clipboard.primary_offer.as_ref()?;
                let mime = offer.with_mime_types(accepted_text_mime)?;
                offer.receive(mime).ok().map(OwnedFd::from)
            }
        }
    }
}

/// Reads a query's answer on a clipboard worker thread, within
/// [`QUERY_READ_TIMEOUT`] and [`MAX_CLIPBOARD_BYTES`]; anything else, or no
/// free worker, answers empty.
fn spawn_osc52_read(pipe: OwnedFd, policy: Osc52Policy, answer: oneshot::Sender<Vec<u8>>) {
    let Some(permit) = try_clipboard_worker(&ACTIVE_CLIPBOARD_WORKERS) else {
        let _ = answer.send(Vec::new());
        return;
    };
    std::thread::spawn(move || {
        let _permit = permit;
        let read = read_clipboard_with_deadline(&pipe, QUERY_READ_TIMEOUT, MAX_CLIPBOARD_BYTES);
        let _ = answer.send(query_answer(policy, read.ok()));
    });
}

/// The answer from a selection this Window owns, under `policy`: its newest
/// source's text, as a Wayland read of it would give. `None` when the Window
/// owns no source for it.
fn own_answer<S>(policy: Osc52Policy, sources: &[(S, Arc<[u8]>)]) -> Option<Vec<u8>> {
    let (_, payload) = sources.last()?;
    Some(query_answer(policy, Some(payload.to_vec())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_selection_this_window_owns_is_answered_from_its_own_copy() {
        let own = [((), Arc::<[u8]>::from(&b"hi"[..]))];
        assert_eq!(
            own_answer(Osc52Policy::CopyPaste, &own),
            Some(b"hi".to_vec())
        );
        // The policy still applies.
        assert_eq!(own_answer(Osc52Policy::Copy, &own), Some(Vec::new()));
        assert_eq!(own_answer(Osc52Policy::Off, &own), Some(Vec::new()));
        // Text only, within the read bound.
        let binary = [((), Arc::<[u8]>::from(&[0xff, 0xfe][..]))];
        assert_eq!(
            own_answer(Osc52Policy::CopyPaste, &binary),
            Some(Vec::new())
        );
        let large = vec![b'x'; MAX_CLIPBOARD_BYTES + 1];
        let large = [((), Arc::<[u8]>::from(large))];
        assert_eq!(own_answer(Osc52Policy::CopyPaste, &large), Some(Vec::new()));
        // Owning nothing, the Window reads the compositor's selection.
        assert_eq!(own_answer::<()>(Osc52Policy::CopyPaste, &[]), None);
    }
}
