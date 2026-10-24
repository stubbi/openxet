//! How long a request body may take to arrive.
//!
//! A body that stalls or trickles would otherwise hold its connection, and
//! the handler waiting on it, for as long as the client likes: an upload
//! refused before its body is read is drained up to the route's limit
//! (ruling M1-R22), so even an unauthenticated client could hold a
//! connection for the length of a 64 MiB body sent a byte at a time.

use std::error::Error as StdError;
use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes, HttpBody};
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use http_body::{Frame, SizeHint};
use tokio::time::{Instant, Sleep, sleep_until};

/// The pace a request body must keep, measured from the handler's first
/// read of it:
///
/// - no gap between two arriving parts may exceed `idle`, and
/// - after a first `idle`, the body must average at least `min_rate` bytes
///   per second: `received` bytes are due by `idle + received / min_rate`
///   after the first read.
///
/// A body that falls behind either ends with an error, which a route
/// answers with `408 body_timeout` and closes the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyDeadline {
    idle: Duration,
    min_rate: u32,
}

impl BodyDeadline {
    /// The default: `idle` 300 s, matching `hf_xet`'s own 300 s read
    /// timeout (S10 §4), and `min_rate` 4 KiB/s, so a 64 MiB xorb may take
    /// up to 4.6 hours over a very slow link but no longer.
    pub const DEFAULT: Self = Self {
        idle: Duration::from_mins(5),
        min_rate: 4096,
    };

    /// A deadline with these bounds, or `None` if either is zero: a zero
    /// `idle` would refuse every body, and a zero rate would leave the total
    /// time unbounded.
    #[must_use]
    pub fn new(idle: Duration, min_rate: u32) -> Option<Self> {
        (!idle.is_zero() && min_rate > 0).then_some(Self { idle, min_rate })
    }

    /// The longest gap allowed between two parts of the body.
    #[must_use]
    pub fn idle(&self) -> Duration {
        self.idle
    }

    /// The least average rate, in bytes per second.
    #[must_use]
    pub fn min_rate(&self) -> u32 {
        self.min_rate
    }

    /// When the body is due to deliver more, given when it was first read,
    /// when it last delivered, and how much it has delivered.
    fn due(&self, first: Instant, last: Instant, received: u64) -> Instant {
        let paced = Duration::from_millis(received.saturating_mul(1000) / u64::from(self.min_rate));
        (last + self.idle).min(first + self.idle + paced)
    }
}

/// How long the server may take to send a dedup or reconstruction answer
/// (rulings M1-R33, M1-R40): `base`, plus the answer's length at
/// `min_rate` bytes per second. When it passes, the answer ends with an
/// error and its memory, with its `dedup` permit or memory reservation, is
/// released, so a client that never reads holds them for at most this
/// long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendDeadline {
    base: Duration,
    min_rate: u64,
}

impl SendDeadline {
    /// The default: 30 s plus 1 MiB/s, so a full 16 MiB dedup answer may
    /// take 46 s and a 42 MB answer for 100k terms 70 s. Even at
    /// 10 Mbit/s either takes under half of that (13 s, 34 s), so only a
    /// client that stalls is cut off, and what it holds is freed well inside
    /// `hf_xet`'s 300 s read timeout (S10 §4).
    pub const DEFAULT: Self = Self {
        base: Duration::from_secs(30),
        min_rate: 1024 * 1024,
    };

    /// A deadline with these bounds, or `None` if either is zero.
    #[must_use]
    pub fn new(base: Duration, min_rate: u64) -> Option<Self> {
        (!base.is_zero() && min_rate > 0).then_some(Self { base, min_rate })
    }

    /// The time allowed to send `len` bytes.
    #[must_use]
    pub fn for_len(&self, len: u64) -> Duration {
        self.base + Duration::from_millis(len.saturating_mul(1000) / self.min_rate)
    }
}

/// The error a body ends with when it falls behind its [`BodyDeadline`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct BodyTimedOut(pub(crate) BodyDeadline);

impl fmt::Display for BodyTimedOut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the request body arrived too slowly")
    }
}

impl StdError for BodyTimedOut {}

/// The [`BodyTimedOut`] in `err` or its sources, if the body failed for
/// falling behind.
pub(crate) fn timed_out(err: &(dyn StdError + 'static)) -> Option<BodyTimedOut> {
    let mut next = Some(err);
    while let Some(err) = next {
        if let Some(timed_out) = err.downcast_ref::<BodyTimedOut>() {
            return Some(*timed_out);
        }
        next = err.source();
    }
    None
}

/// Middleware putting every request body under `deadline`.
pub(crate) async fn limit(
    State(deadline): State<BodyDeadline>,
    request: Request,
    next: Next,
) -> Response {
    next.run(request.map(|body| Body::new(DeadlineBody::new(body, deadline))))
        .await
}

/// A body that ends with [`BodyTimedOut`] once it falls behind its
/// deadline. The clock starts at the first read, so time a handler spends
/// before reading (authentication, waiting for a permit) is not counted.
struct DeadlineBody {
    inner: Body,
    deadline: BodyDeadline,
    /// The first read and the last arrival, once read.
    times: Option<(Instant, Instant)>,
    received: u64,
    timer: Option<Pin<Box<Sleep>>>,
    expired: bool,
}

impl DeadlineBody {
    fn new(inner: Body, deadline: BodyDeadline) -> Self {
        Self {
            inner,
            deadline,
            times: None,
            received: 0,
            timer: None,
            expired: false,
        }
    }
}

impl HttpBody for DeadlineBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        let this = self.get_mut();
        if this.expired {
            return Poll::Ready(None);
        }
        let now = Instant::now();
        let (first, last) = *this.times.get_or_insert((now, now));
        let deadline = this.deadline;
        let received = this.received;
        let timer = this
            .timer
            .get_or_insert_with(|| Box::pin(sleep_until(deadline.due(first, last, received))));

        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    this.received = this.received.saturating_add(data.len() as u64);
                }
                this.times = Some((first, now));
                timer
                    .as_mut()
                    .reset(deadline.due(first, now, this.received));
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(other) => Poll::Ready(other),
            Poll::Pending => match timer.as_mut().poll(cx) {
                Poll::Ready(()) => {
                    this.expired = true;
                    Poll::Ready(Some(Err(axum::Error::new(BodyTimedOut(deadline)))))
                }
                Poll::Pending => Poll::Pending,
            },
        }
    }

    fn is_end_stream(&self) -> bool {
        self.expired || self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use http_body_util::BodyExt as _;
    use http_body_util::channel::{Channel, Sender};

    use super::*;

    /// A body whose parts the test sends by hand, under `deadline`.
    fn channel_body(deadline: BodyDeadline) -> (Sender<Bytes>, DeadlineBody) {
        let (tx, body) = Channel::<Bytes>::new(16);
        (tx, DeadlineBody::new(Body::new(body), deadline))
    }

    async fn next(body: &mut DeadlineBody) -> Option<Result<Bytes, axum::Error>> {
        body.frame()
            .await
            .map(|frame| frame.map(|f| f.into_data().unwrap()))
    }

    const SECOND: Duration = Duration::from_secs(1);

    #[tokio::test(start_paused = true)]
    async fn a_body_that_keeps_pace_is_read_in_full() {
        let deadline = BodyDeadline::new(10 * SECOND, 100).unwrap();
        let (mut tx, mut body) = channel_body(deadline);
        let sender = tokio::spawn(async move {
            for _ in 0..20 {
                tokio::time::sleep(9 * SECOND).await;
                tx.send_data(Bytes::from(vec![0; 1000])).await.unwrap();
            }
        });
        let mut read = 0;
        while let Some(part) = next(&mut body).await {
            read += part.unwrap().len();
        }
        sender.await.unwrap();
        assert_eq!(read, 20_000);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_body_ends_after_idle() {
        let deadline = BodyDeadline::new(10 * SECOND, 1).unwrap();
        let (mut tx, mut body) = channel_body(deadline);
        tx.send_data(Bytes::from_static(b"abc")).await.unwrap();
        assert_eq!(next(&mut body).await.unwrap().unwrap(), "abc");

        let started = Instant::now();
        let err = next(&mut body).await.unwrap().unwrap_err();
        assert_eq!(started.elapsed(), 10 * SECOND);
        assert!(timed_out(&err).is_some());
        assert!(next(&mut body).await.is_none());
        drop(tx);
    }

    #[tokio::test(start_paused = true)]
    async fn a_trickling_body_ends_when_it_falls_below_the_rate() {
        // One byte every 5 s never trips the 10 s idle bound, but 100 B/s
        // allows only 10 s plus 10 ms per byte in total.
        let deadline = BodyDeadline::new(10 * SECOND, 100).unwrap();
        let (mut tx, mut body) = channel_body(deadline);
        let started = Instant::now();
        tokio::spawn(async move {
            loop {
                if tx.send_data(Bytes::from_static(b"x")).await.is_err() {
                    break;
                }
                tokio::time::sleep(5 * SECOND).await;
            }
        });
        let mut read = 0;
        let err = loop {
            match next(&mut body).await.unwrap() {
                Ok(part) => read += part.len(),
                Err(err) => break err,
            }
        };
        assert!(timed_out(&err).is_some());
        assert_eq!(
            read, 3,
            "bytes at 0, 5 and 10 s arrive before the 10.03 s deadline"
        );
        assert!(started.elapsed() < 11 * SECOND);
    }

    #[tokio::test(start_paused = true)]
    async fn the_clock_starts_at_the_first_read() {
        let deadline = BodyDeadline::new(10 * SECOND, 1).unwrap();
        let (mut tx, mut body) = channel_body(deadline);
        tokio::time::sleep(60 * SECOND).await;
        tx.send_data(Bytes::from_static(b"late")).await.unwrap();
        assert_eq!(next(&mut body).await.unwrap().unwrap(), "late");
    }
}
