//! The body of a built answer, which holds what bounds it (a dedup permit,
//! a memory reservation) until it has been sent, dropped, or overrun its
//! send deadline (rulings M1-R32, M1-R33, M1-R40).
//!
//! The answer is handed to the server in copies of at most
//! [`FRAME_BYTES`], so what the server buffers for a slow client never
//! references the answer's buffers. A timer task frees the buffers and
//! the guard when the deadline passes, even if the server never polls the
//! body again (it doesn't while the client's socket is full); the next
//! poll then fails the body, which closes the connection.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes};
use http_body::{Frame, SizeHint};
use tokio::task::JoinHandle;

/// The largest frame copied out of an answer: small enough that a stalled
/// client holds only a few frames in the server's buffers.
const FRAME_BYTES: usize = 64 * 1024;

/// `parts`, in order, as a response body holding `guard`, which is dropped
/// with the parts when the body has been sent, is dropped, or `deadline`
/// passes. The Hub sends a Git blob it reserved memory for with it.
pub fn send_body<G: Send + 'static>(parts: Vec<Bytes>, guard: G, deadline: Duration) -> Body {
    let len = parts.iter().map(|part| part.len() as u64).sum();
    let parts: VecDeque<Bytes> = parts.into_iter().filter(|p| !p.is_empty()).collect();
    let state = if parts.is_empty() {
        State::Done
    } else {
        State::Sending {
            parts,
            _guard: guard,
        }
    };
    let state = Arc::new(Mutex::new(state));
    let weak = Arc::downgrade(&state);
    let timer = tokio::spawn(async move {
        tokio::time::sleep(deadline).await;
        if let Some(state) = weak.upgrade() {
            let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
            if matches!(*state, State::Sending { .. }) {
                *state = State::Expired;
                tracing::debug!("an answer missed its send deadline; its memory is released");
            }
        }
    });
    Body::new(SendBody { state, len, timer })
}

enum State<G> {
    Sending {
        /// What is left to send, none of it empty.
        parts: VecDeque<Bytes>,
        _guard: G,
    },
    Done,
    Expired,
}

struct SendBody<G> {
    state: Arc<Mutex<State<G>>>,
    len: u64,
    timer: JoinHandle<()>,
}

impl<G> Drop for SendBody<G> {
    fn drop(&mut self) {
        self.timer.abort();
    }
}

/// The error an answer ends with when it misses its send deadline.
#[derive(Debug)]
struct SendTimedOut;

impl std::fmt::Display for SendTimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the answer was not sent within its deadline")
    }
}

impl std::error::Error for SendTimedOut {}

impl<G: Send + 'static> http_body::Body for SendBody<G> {
    type Data = Bytes;
    type Error = SendTimedOut;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, SendTimedOut>>> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let frame = match &mut *state {
            State::Sending { parts, .. } => {
                let Some(part) = parts.front_mut() else {
                    unreachable!("a sending answer has a part left");
                };
                let n = part.len().min(FRAME_BYTES);
                let frame = Bytes::copy_from_slice(&part[..n]);
                *part = part.slice(n..);
                if part.is_empty() {
                    parts.pop_front();
                }
                if parts.is_empty() {
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
    async fn an_unsent_answer_releases_its_guard_at_the_deadline() {
        // Rulings M1-R33, M1-R40: a client that never reads loses the
        // guard, and the body's memory, when the send deadline passes,
        // whether or not the server polls the body again; the body then
        // ends with an error.
        let pool = Arc::new(Semaphore::new(1));
        let permit = Arc::clone(&pool).acquire_owned().await.unwrap();
        let body = send_body(
            vec![Bytes::from(vec![7; 300_000])],
            permit,
            Duration::from_millis(200),
        );
        assert_eq!(pool.available_permits(), 0);

        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(
            pool.available_permits(),
            1,
            "the guard outlived the deadline"
        );
        assert!(axum::body::to_bytes(body, usize::MAX).await.is_err());
    }

    #[tokio::test]
    async fn a_sent_answer_releases_its_guard_when_it_ends_or_is_dropped() {
        let pool = Arc::new(Semaphore::new(1));
        let data: Vec<u8> = (0..300_000u32).map(|i| i.to_le_bytes()[0]).collect();
        let parts = vec![
            Bytes::copy_from_slice(&data[..100]),
            Bytes::new(),
            Bytes::copy_from_slice(&data[100..200_000]),
            Bytes::copy_from_slice(&data[200_000..]),
        ];
        let permit = Arc::clone(&pool).acquire_owned().await.unwrap();
        let body = send_body(parts.clone(), permit, Duration::from_mins(1));
        assert_eq!(
            http_body::Body::size_hint(&body).exact(),
            Some(data.len() as u64)
        );
        let sent = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        assert_eq!(sent, data);
        assert_eq!(pool.available_permits(), 1);

        let permit = Arc::clone(&pool).acquire_owned().await.unwrap();
        drop(send_body(parts, permit, Duration::from_mins(1)));
        assert_eq!(pool.available_permits(), 1);
    }

    #[tokio::test]
    async fn an_empty_answer_is_done_at_once() {
        let pool = Arc::new(Semaphore::new(1));
        let permit = Arc::clone(&pool).acquire_owned().await.unwrap();
        let body = send_body(Vec::new(), permit, Duration::from_mins(1));
        assert_eq!(pool.available_permits(), 1);
        assert!(
            axum::body::to_bytes(body, usize::MAX)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
