//! The body of a dedup answer, which holds its `dedup` permit until it has
//! been sent, dropped, or overrun its send deadline (rulings M1-R32,
//! M1-R33).
//!
//! The answer is handed to the server in copies of at most
//! [`FRAME_BYTES`], so what the server buffers for a slow client never
//! references the answer's buffer. A timer task frees the buffer and the
//! permit when the deadline passes, even if the server never polls the
//! body again (it doesn't while the client's socket is full); the next
//! poll then fails the body, which closes the connection.

use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes};
use http_body::{Frame, SizeHint};
use tokio::sync::OwnedSemaphorePermit;
use tokio::task::JoinHandle;

/// The largest frame copied out of an answer: small enough that a stalled
/// client holds only a few frames in the server's buffers.
const FRAME_BYTES: usize = 64 * 1024;

/// `body` as a response body holding `permit`, which is released when
/// the body has been sent, is dropped, or `deadline` passes.
pub(crate) fn answer_body(body: Vec<u8>, permit: OwnedSemaphorePermit, deadline: Duration) -> Body {
    let len = body.len() as u64;
    let state = Arc::new(Mutex::new(State::Sending {
        body,
        sent: 0,
        _permit: permit,
    }));
    let weak = Arc::downgrade(&state);
    let timer = tokio::spawn(async move {
        tokio::time::sleep(deadline).await;
        if let Some(state) = weak.upgrade() {
            let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
            if matches!(*state, State::Sending { .. }) {
                *state = State::Expired;
                tracing::debug!("a dedup answer missed its send deadline; its permit is released");
            }
        }
    });
    Body::new(AnswerBody { state, len, timer })
}

enum State {
    Sending {
        body: Vec<u8>,
        sent: usize,
        _permit: OwnedSemaphorePermit,
    },
    Done,
    Expired,
}

struct AnswerBody {
    state: Arc<Mutex<State>>,
    len: u64,
    timer: JoinHandle<()>,
}

impl Drop for AnswerBody {
    fn drop(&mut self) {
        self.timer.abort();
    }
}

/// The error an answer ends with when it misses its send deadline.
#[derive(Debug)]
struct SendTimedOut;

impl std::fmt::Display for SendTimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the dedup answer was not sent within its deadline")
    }
}

impl std::error::Error for SendTimedOut {}

impl http_body::Body for AnswerBody {
    type Data = Bytes;
    type Error = SendTimedOut;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, SendTimedOut>>> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let frame = match &mut *state {
            State::Sending { body, sent, .. } => {
                let end = body.len().min(*sent + FRAME_BYTES);
                let frame = Bytes::copy_from_slice(&body[*sent..end]);
                *sent = end;
                if end == body.len() {
                    *state = State::Done;
                }
                frame
            }
            State::Done => return Poll::Ready(None),
            State::Expired => return Poll::Ready(Some(Err(SendTimedOut))),
        };
        Poll::Ready(Some(Ok(Frame::data(frame))))
    }

    fn is_end_stream(&self) -> bool {
        matches!(
            *self.state.lock().unwrap_or_else(PoisonError::into_inner),
            State::Done
        )
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.len)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::Arc;

    use tokio::sync::Semaphore;

    use super::*;

    #[tokio::test]
    async fn an_unsent_answer_releases_its_permit_at_the_deadline() {
        // Ruling M1-R33: a client that never reads loses its permit, and the
        // body's memory, when the send deadline passes, whether or not the
        // server polls the body again; the body then ends with an error.
        let pool = Arc::new(Semaphore::new(1));
        let permit = Arc::clone(&pool).acquire_owned().await.unwrap();
        let body = answer_body(vec![7; 300_000], permit, Duration::from_millis(200));
        assert_eq!(pool.available_permits(), 0);

        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(
            pool.available_permits(),
            1,
            "the permit outlived the deadline"
        );
        assert!(axum::body::to_bytes(body, usize::MAX).await.is_err());
    }

    #[tokio::test]
    async fn a_sent_answer_releases_its_permit_when_it_ends_or_is_dropped() {
        let pool = Arc::new(Semaphore::new(1));
        let data: Vec<u8> = (0..300_000u32).map(|i| i.to_le_bytes()[0]).collect();
        let permit = Arc::clone(&pool).acquire_owned().await.unwrap();
        let body = answer_body(data.clone(), permit, Duration::from_mins(1));
        assert_eq!(
            http_body::Body::size_hint(&body).exact(),
            Some(data.len() as u64)
        );
        let sent = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        assert_eq!(sent, data);
        assert_eq!(pool.available_permits(), 1);

        let permit = Arc::clone(&pool).acquire_owned().await.unwrap();
        drop(answer_body(data, permit, Duration::from_mins(1)));
        assert_eq!(pool.available_permits(), 1);
    }
}
