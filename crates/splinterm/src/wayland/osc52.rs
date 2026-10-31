//! The Window side of OSC 52 (Plan 0074 M2).
//!
//! The panes this Window controls hand it their programs' clipboard sets
//! and queries through its [`crate::osc52::Osc52Hub`]. Once per event-loop
//! turn the Window publishes each allowed set as its own selection, with
//! its newest input serial, and answers each query from its clipboard or
//! primary selection: text only, read off the event loop within a bound.
//! What `clipboard.osc52` refuses is answered empty.

use std::os::fd::OwnedFd;

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
