//! Reaching the rest of a request body that a refusal left unread (decision
//! 0048). A `413` is produced deep inside a request, by an extractor, the MCP
//! SDK or a handler, after the body was handed over and dropped. The request
//! body is therefore wrapped on admission: when the wrapper is dropped before
//! the end of the body, it gives the rest back through a channel that only
//! this request owns, so the admission middleware can drain it before it
//! answers.

use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::{Body, BodyDataStream, Bytes, HttpBody};
use axum::extract::Request;
use futures_core::Stream;
use tokio::sync::oneshot;

/// What a request's handlers left of its body, which can still be read.
#[derive(Debug)]
pub(super) struct Unread(oneshot::Receiver<BodyDataStream>);

impl Unread {
    /// Wraps the body of `request` and returns the way back to what is left
    /// of it, or `None` for a request that has no body to read.
    pub(super) fn watch(request: &mut Request) -> Option<Self> {
        if request.body().is_end_stream() {
            return None;
        }
        let (hand_back, unread) = oneshot::channel();
        let data = std::mem::take(request.body_mut()).into_data_stream();
        let watched = Watched {
            data: Some(data),
            hand_back: Some(hand_back),
        };
        *request.body_mut() = Body::from_stream(watched);
        Some(Self(unread))
    }

    /// The rest of the body, if the handlers dropped it before its end. It is
    /// `None` when the body was read to its end, failed, or is still held
    /// somewhere, such as by a response.
    pub(super) fn into_rest(mut self) -> Option<BodyDataStream> {
        self.0.try_recv().ok()
    }
}

/// The body as handlers read it. It passes the data through unchanged and
/// remembers whether the end was reached.
#[derive(Debug)]
struct Watched {
    /// The body until it ends or fails; `None` afterwards, when nothing is
    /// left to give back.
    data: Option<BodyDataStream>,
    hand_back: Option<oneshot::Sender<BodyDataStream>>,
}

impl Stream for Watched {
    type Item = Result<Bytes, axum::Error>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let Some(data) = this.data.as_mut() else {
            return Poll::Ready(None);
        };
        let polled = Pin::new(data).poll_next(context);
        if matches!(polled, Poll::Ready(None | Some(Err(_)))) {
            this.data = None;
        }
        polled
    }
}

impl Drop for Watched {
    fn drop(&mut self) {
        if let (Some(data), Some(hand_back)) = (self.data.take(), self.hand_back.take()) {
            // A closed channel means that nobody waits for the rest.
            drop(hand_back.send(data));
        }
    }
}
