//! Reaching the rest of a request body that a refusal left unread (decision
//! 0048). A `413` is produced deep inside a request, by an extractor, the MCP
//! SDK or a handler, after the body was handed over and dropped. The request
//! body is therefore wrapped on admission: when the wrapper is dropped before
//! the end of the body, it gives the rest back through a channel that only
//! this request owns, so the admission middleware can drain it before it
//! answers.
//!
//! The wrapper forwards every frame, the size hint and the end-of-stream
//! report of the body it wraps, so what handlers see does not change.

use std::mem;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use axum::extract::Request;
use http_body::{Body as HttpBody, Frame, SizeHint};
use tokio::sync::oneshot;

/// The way back to what a request's handlers left of its body.
///
/// The rest is handed back whenever the wrapped body is dropped before its
/// end, for any response, and then waits in the channel until the request's
/// task ends. That holds the unread body for as long as the handler runs and
/// has no effect on the response.
#[derive(Debug)]
pub(super) struct Unread(oneshot::Receiver<Body>);

impl Unread {
    /// Wraps the body of `request` and returns the way back to what is left
    /// of it, or `None` for a request that has no body to read.
    pub(super) fn watch(request: &mut Request) -> Option<Self> {
        if request.body().is_end_stream() {
            return None;
        }
        let (hand_back, unread) = oneshot::channel();
        let inner = mem::take(request.body_mut());
        *request.body_mut() = Body::new(Watched {
            inner: Some(inner),
            hand_back: Some(hand_back),
        });
        Some(Self(unread))
    }

    /// The rest of the body, if the handlers dropped it before its end.
    ///
    /// It is `None` when the body was read to its end or failed, and also
    /// when the body is still held somewhere when this is called, such as in
    /// a response or a spawned task: such a `413` is sent without a drain.
    /// The per-path `413` tests in `tests/request_limits.rs` (MCP, owner API,
    /// frame and room routes) are what would show a dependency upgrade that
    /// starts holding the body longer.
    pub(super) fn into_rest(mut self) -> Option<Body> {
        self.0.try_recv().ok()
    }
}

/// The body as handlers read it. It remembers whether the end was reached or
/// a read failed, and gives the body back if it is dropped before that.
#[derive(Debug)]
struct Watched {
    /// The body until it ends or fails; `None` afterwards, when nothing is
    /// left to give back.
    inner: Option<Body>,
    hand_back: Option<oneshot::Sender<Body>>,
}

impl HttpBody for Watched {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        let this = self.get_mut();
        let Some(inner) = this.inner.as_mut() else {
            return Poll::Ready(None);
        };
        let polled = Pin::new(inner).poll_frame(context);
        if matches!(polled, Poll::Ready(None | Some(Err(_)))) {
            this.inner = None;
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.as_ref().is_none_or(Body::is_end_stream)
    }

    fn size_hint(&self) -> SizeHint {
        self.inner
            .as_ref()
            .map_or_else(|| SizeHint::with_exact(0), Body::size_hint)
    }
}

impl Drop for Watched {
    fn drop(&mut self) {
        let (Some(inner), Some(hand_back)) = (self.inner.take(), self.hand_back.take()) else {
            return;
        };
        if !inner.is_end_stream() {
            // A closed channel means that nobody waits for the rest.
            drop(hand_back.send(inner));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::future::poll_fn;
    use std::io;

    use axum::http::{HeaderMap, HeaderValue};

    use super::*;

    /// A body that serves the scripted frames. Like a chunked request it
    /// does not know where its end is, unless `knows_its_end` is set.
    struct Script {
        frames: VecDeque<Result<Frame<Bytes>, axum::Error>>,
        hint: SizeHint,
        knows_its_end: bool,
    }

    impl Script {
        fn data(chunks: &[&'static str]) -> Self {
            Self {
                frames: chunks
                    .iter()
                    .map(|chunk| Ok(Frame::data(Bytes::from_static(chunk.as_bytes()))))
                    .collect(),
                hint: SizeHint::default(),
                knows_its_end: false,
            }
        }

        fn watched(self) -> (Request, Unread) {
            let mut request = Request::new(Body::new(self));
            let unread = Unread::watch(&mut request)
                .unwrap_or_else(|| panic!("a body with frames is watched"));
            (request, unread)
        }
    }

    impl HttpBody for Script {
        type Data = Bytes;
        type Error = axum::Error;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
            Poll::Ready(self.frames.pop_front())
        }

        fn is_end_stream(&self) -> bool {
            self.knows_its_end && self.frames.is_empty()
        }

        fn size_hint(&self) -> SizeHint {
            self.hint
        }
    }

    async fn next(body: &mut Body) -> Option<Result<Frame<Bytes>, axum::Error>> {
        poll_fn(|context| Pin::new(&mut *body).poll_frame(context)).await
    }

    async fn next_data(body: &mut Body) -> Option<Bytes> {
        match next(body).await {
            Some(Ok(frame)) => frame.into_data().ok(),
            _ => None,
        }
    }

    #[tokio::test]
    async fn a_body_read_to_its_end_hands_nothing_back() {
        let (request, unread) = Script::data(&["ab", "cd"]).watched();
        let mut body = request.into_body();
        assert_eq!(next_data(&mut body).await.as_deref(), Some(&b"ab"[..]));
        assert_eq!(next_data(&mut body).await.as_deref(), Some(&b"cd"[..]));
        assert!(next(&mut body).await.is_none(), "the end must be reported");
        drop(body);
        assert!(unread.into_rest().is_none());
    }

    #[tokio::test]
    async fn a_body_that_failed_hands_nothing_back() {
        let mut script = Script::data(&["ab"]);
        script
            .frames
            .push_back(Err(axum::Error::new(io::Error::from(
                io::ErrorKind::ConnectionReset,
            ))));
        script.frames.push_back(Ok(Frame::data(Bytes::from("zz"))));
        let (request, unread) = script.watched();
        let mut body = request.into_body();
        assert!(next_data(&mut body).await.is_some());
        assert!(matches!(next(&mut body).await, Some(Err(_))));
        drop(body);
        assert!(unread.into_rest().is_none());
    }

    #[tokio::test]
    async fn a_body_dropped_before_its_end_hands_back_the_rest() {
        let (request, unread) = Script::data(&["ab", "cd", "ef"]).watched();
        let mut body = request.into_body();
        assert_eq!(next_data(&mut body).await.as_deref(), Some(&b"ab"[..]));
        drop(body);
        let mut rest = unread
            .into_rest()
            .unwrap_or_else(|| panic!("the rest comes back"));
        assert_eq!(next_data(&mut rest).await.as_deref(), Some(&b"cd"[..]));
        assert_eq!(next_data(&mut rest).await.as_deref(), Some(&b"ef"[..]));
        assert!(next(&mut rest).await.is_none());
    }

    #[tokio::test]
    async fn the_rest_is_not_available_while_the_body_is_still_held() {
        let (request, unread) = Script::data(&["ab"]).watched();
        assert!(unread.into_rest().is_none());
        drop(request);
    }

    #[tokio::test]
    async fn size_hint_end_of_stream_and_trailers_are_forwarded() {
        let mut trailers = HeaderMap::new();
        trailers.insert("x-checksum", HeaderValue::from_static("7"));
        let mut script = Script::data(&["abc"]);
        script.frames.push_back(Ok(Frame::trailers(trailers)));
        script.hint = SizeHint::with_exact(3);
        script.knows_its_end = true;
        let (request, _unread) = script.watched();
        let mut body = request.into_body();
        assert_eq!(body.size_hint().exact(), Some(3));
        assert!(!body.is_end_stream());
        assert!(next_data(&mut body).await.is_some());
        let Some(Ok(frame)) = next(&mut body).await else {
            panic!("the trailers frame must follow the data");
        };
        let Ok(trailers) = frame.into_trailers() else {
            panic!("the frame must be the trailers");
        };
        assert_eq!(
            trailers.get("x-checksum").map(HeaderValue::as_bytes),
            Some(&b"7"[..])
        );
        assert!(body.is_end_stream(), "the end must be reported at once");
    }

    #[tokio::test]
    async fn a_request_without_a_body_is_left_alone() {
        let mut request = Request::new(Body::empty());
        assert!(Unread::watch(&mut request).is_none());
        assert!(request.body().is_end_stream());
    }
}
