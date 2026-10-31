//! OSC 52 clipboard requests a Window's panes hand its frontend (Plan 0074
//! M2).
//!
//! A program in a pane sets or reads the clipboard with OSC 52. The daemon
//! sends each request only to the Window that controls the pane, on that
//! pane's control subscription. The pane's controller hands it to the
//! Window's [`Osc52Hub`]; the frontend publishes a set as its own clipboard
//! selection and answers a query with its clipboard text, as
//! `clipboard.osc52` allows.
//!
//! A query's answer always comes back: the text, or nothing when the
//! configuration refuses, the clipboard holds no text, or the read takes too
//! long. The program then gets the empty reply.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, PoisonError},
    task::Waker,
    time::Duration,
};

use splinterm_protocol::ClipboardSelection;
use tokio::sync::oneshot;

/// How long a pane waits for its Window's answer before answering empty.
/// Shorter than the daemon's own wait, so the program hears the answer.
pub const QUERY_ANSWER_TIMEOUT: Duration = Duration::from_millis(1_500);

/// How long the frontend reads its clipboard for one query.
pub const QUERY_READ_TIMEOUT: Duration = Duration::from_millis(1_200);

/// Requests the hub holds for the frontend. Sets are coalesced per
/// selection; past this, the oldest request is dropped, and a dropped
/// query is answered empty.
const MAX_QUEUED_REQUESTS: usize = 8;

/// What programs in a Window's panes may do with its clipboard through
/// OSC 52: `clipboard.osc52`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Osc52Policy {
    /// Programs may set and read the clipboard.
    #[default]
    CopyPaste,
    /// Programs may set the clipboard; a read gets nothing.
    Copy,
    /// Programs may do neither; a read gets nothing.
    Off,
}

impl Osc52Policy {
    /// Parses a `clipboard.osc52` value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "copy-paste" => Some(Self::CopyPaste),
            "copy" => Some(Self::Copy),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    /// Whether a program may set the clipboard.
    #[must_use]
    pub const fn allows_set(self) -> bool {
        matches!(self, Self::CopyPaste | Self::Copy)
    }

    /// Whether a program may read the clipboard.
    #[must_use]
    pub const fn allows_read(self) -> bool {
        matches!(self, Self::CopyPaste)
    }
}

/// One program request for the frontend.
#[derive(Debug)]
pub enum Osc52Request {
    /// Publish `text` as `selection`.
    Set {
        selection: ClipboardSelection,
        text: String,
    },
    /// Answer with `selection`'s text through `answer`; dropping it answers
    /// empty.
    Query {
        selection: ClipboardSelection,
        answer: oneshot::Sender<Vec<u8>>,
    },
}

impl Osc52Request {
    fn sets(&self, selection: ClipboardSelection) -> bool {
        matches!(self, Self::Set { selection: set, .. } if *set == selection)
    }
}

#[derive(Debug, Default)]
struct HubInner {
    queue: Mutex<VecDeque<Osc52Request>>,
    waker: Mutex<Option<Waker>>,
}

/// One Window's OSC 52 requests, shared by its panes' controllers and its
/// frontend. Clones share the same queue. Until a frontend installs its
/// waker, as for a client without a Window, requests are dropped at once:
/// sets are lost and queries answered empty.
#[derive(Clone, Debug, Default)]
pub struct Osc52Hub {
    inner: Arc<HubInner>,
}

impl Osc52Hub {
    /// Wakes the frontend's event loop whenever a request arrives.
    pub fn set_waker(&self, waker: Waker) {
        *self
            .inner
            .waker
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(waker);
    }

    /// Hands a set from the daemon to the frontend. `data` is base64 text;
    /// anything else is dropped.
    pub fn set(&self, selection: ClipboardSelection, data: &str) {
        let Some(text) = splinterm_protocol::decode_clipboard_data(data)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .filter(|text| !text.is_empty())
        else {
            return;
        };
        self.push(Osc52Request::Set { selection, text });
    }

    /// Hands a query to the frontend; the receiver yields its answer, or an
    /// error when the frontend dropped it, which means empty.
    #[must_use]
    pub fn query(&self, selection: ClipboardSelection) -> oneshot::Receiver<Vec<u8>> {
        let (answer, receiver) = oneshot::channel();
        self.push(Osc52Request::Query { selection, answer });
        receiver
    }

    /// Takes every waiting request, oldest first.
    #[must_use]
    pub fn drain(&self) -> Vec<Osc52Request> {
        self.queue().drain(..).collect()
    }

    fn push(&self, request: Osc52Request) {
        let waker = self
            .inner
            .waker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let Some(waker) = waker else {
            return;
        };
        {
            let mut queue = self.queue();
            if let Osc52Request::Set { selection, .. } = &request {
                let selection = *selection;
                queue.retain(|queued| !queued.sets(selection));
            }
            if queue.len() >= MAX_QUEUED_REQUESTS {
                queue.pop_front();
            }
            queue.push_back(request);
        }
        waker.wake();
    }

    fn queue(&self) -> std::sync::MutexGuard<'_, VecDeque<Osc52Request>> {
        self.inner
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// The text a query answers with under `policy`: the clipboard's text, or
/// nothing when the policy refuses or the clipboard read gave no UTF-8 text.
#[must_use]
pub fn query_answer(policy: Osc52Policy, read: Option<Vec<u8>>) -> Vec<u8> {
    if !policy.allows_read() {
        return Vec::new();
    }
    read.filter(|bytes| {
        bytes.len() <= splinterm_protocol::MAX_CLIPBOARD_BYTES && std::str::from_utf8(bytes).is_ok()
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::task::Wake;

    use super::*;

    struct Count(Mutex<usize>);

    impl Wake for Count {
        fn wake(self: Arc<Self>) {
            *self.0.lock().unwrap() += 1;
        }
    }

    fn hub() -> (Osc52Hub, Arc<Count>) {
        let hub = Osc52Hub::default();
        let count = Arc::new(Count(Mutex::new(0)));
        hub.set_waker(Waker::from(Arc::clone(&count)));
        (hub, count)
    }

    #[test]
    fn policies_parse_and_gate_sets_and_reads() {
        assert_eq!(Osc52Policy::default(), Osc52Policy::CopyPaste);
        assert_eq!(
            Osc52Policy::parse("copy-paste"),
            Some(Osc52Policy::CopyPaste)
        );
        assert_eq!(Osc52Policy::parse("copy"), Some(Osc52Policy::Copy));
        assert_eq!(Osc52Policy::parse("off"), Some(Osc52Policy::Off));
        assert_eq!(Osc52Policy::parse("yes"), None);
        assert_eq!(Osc52Policy::parse("Copy"), None);
        assert!(Osc52Policy::CopyPaste.allows_set() && Osc52Policy::CopyPaste.allows_read());
        assert!(Osc52Policy::Copy.allows_set() && !Osc52Policy::Copy.allows_read());
        assert!(!Osc52Policy::Off.allows_set() && !Osc52Policy::Off.allows_read());
    }

    #[test]
    fn a_query_is_answered_only_with_text_the_policy_allows() {
        let text = Some(b"hi".to_vec());
        assert_eq!(query_answer(Osc52Policy::CopyPaste, text.clone()), b"hi");
        assert!(query_answer(Osc52Policy::Copy, text.clone()).is_empty());
        assert!(query_answer(Osc52Policy::Off, text).is_empty());
        assert!(query_answer(Osc52Policy::CopyPaste, None).is_empty());
        assert!(query_answer(Osc52Policy::CopyPaste, Some(vec![0xff, 0xfe])).is_empty());
        let oversized = vec![b'x'; splinterm_protocol::MAX_CLIPBOARD_BYTES + 1];
        assert!(query_answer(Osc52Policy::CopyPaste, Some(oversized)).is_empty());
    }

    #[test]
    fn sets_are_decoded_text_and_the_latest_per_selection_wins() {
        let (hub, wakes) = hub();
        hub.set(ClipboardSelection::Clipboard, "aGk=");
        hub.set(ClipboardSelection::Primary, "cA==");
        hub.set(ClipboardSelection::Clipboard, "Ynll");
        // Not base64, not UTF-8, or empty: dropped.
        hub.set(ClipboardSelection::Clipboard, "!!");
        hub.set(ClipboardSelection::Clipboard, "//4=");
        hub.set(ClipboardSelection::Clipboard, "");
        assert_eq!(*wakes.0.lock().unwrap(), 3);
        let texts = hub
            .drain()
            .into_iter()
            .map(|request| match request {
                Osc52Request::Set { selection, text } => (selection, text),
                Osc52Request::Query { .. } => panic!("no query was made"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            texts,
            [
                (ClipboardSelection::Primary, "p".to_owned()),
                (ClipboardSelection::Clipboard, "bye".to_owned()),
            ]
        );
        assert!(hub.drain().is_empty());
    }

    #[tokio::test]
    async fn queries_without_a_frontend_or_beyond_the_bound_are_answered_empty() {
        let detached = Osc52Hub::default();
        assert!(detached.query(ClipboardSelection::Clipboard).await.is_err());
        detached.set(ClipboardSelection::Clipboard, "aGk=");
        assert!(detached.drain().is_empty());

        let (hub, _) = hub();
        let first = hub.query(ClipboardSelection::Clipboard);
        let rest = (0..MAX_QUEUED_REQUESTS)
            .map(|_| hub.query(ClipboardSelection::Primary))
            .collect::<Vec<_>>();
        // The oldest query was dropped to keep the bound.
        assert!(first.await.is_err());
        let queued = hub.drain();
        assert_eq!(queued.len(), MAX_QUEUED_REQUESTS);
        for request in queued {
            let Osc52Request::Query { answer, .. } = request else {
                panic!("only queries were made");
            };
            answer.send(b"x".to_vec()).unwrap();
        }
        for receiver in rest {
            assert_eq!(receiver.await.unwrap(), b"x");
        }
    }
}
