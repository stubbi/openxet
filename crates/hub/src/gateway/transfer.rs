//! A file range read for the gateway: the plan's fetches in order, each
//! streamed from the bucket and decoded one chunk at a time (ruling
//! M1-R27), sent as a response body that holds the transfer's memory
//! reservation until it is sent, dropped, or goes idle (ruling M1-R40).

use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use axum::body::{Body, Bytes};
use futures_util::StreamExt as _;
use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use openxet_cas::{Fetch, Reservation};
use openxet_protocol::{
    CHUNK_HEADER_BYTES, MAX_CHUNK_BYTES, ProtocolError, decode_stored_chunk, stored_chunk_len,
};
use openxet_storage::{Bucket, Key, STREAM_BLOCK_BYTES, StorageError};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::HubError;

/// What a transfer holds besides its fetches, reserved with them: one
/// stream block, one stored chunk (compressed, at most twice its decoded
/// size as upload validation allows) and one decoded chunk.
pub(super) const WORKING_BYTES: u64 = STREAM_BLOCK_BYTES as u64 + 3 * MAX_CHUNK_BYTES;

/// A fetch's stored bytes, in blocks.
pub(super) type Blocks = BoxStream<'static, Result<Bytes, StorageError>>;

/// Opens a fetch's stored bytes: the bucket's range stream in production.
pub(super) type Opener =
    Box<dyn FnMut(Fetch) -> BoxFuture<'static, Result<Blocks, StorageError>> + Send>;

/// The [`Opener`] reading `bucket`'s xorbs.
pub(super) fn bucket_opener(bucket: Arc<dyn Bucket>) -> Opener {
    Box::new(move |fetch: Fetch| {
        let bucket = Arc::clone(&bucket);
        Box::pin(async move {
            let stream = bucket
                .get_range_stream(&Key::Xorb(fetch.xorb), fetch.bytes.0, fetch.bytes.1)
                .await?;
            Ok(stream.blocks)
        })
    })
}

/// Why a transfer stopped partway.
#[derive(Debug, thiserror::Error)]
pub(super) enum TransferError {
    #[error("the bucket failed: {0}")]
    Storage(#[from] StorageError),
    #[error("a stored chunk is invalid: {0}")]
    Chunk(#[from] ProtocolError),
    #[error("a fetched range ends inside a chunk")]
    Truncated,
    #[error("the plan's fetches ended before the range did")]
    ShortPlan,
}

impl From<TransferError> for HubError {
    fn from(e: TransferError) -> Self {
        match e {
            TransferError::Storage(e) => e.into(),
            e => Self::Internal(Box::new(e)),
        }
    }
}

/// A file range being read: `skip` bytes dropped from the start of the
/// fetches' decoded chunks, then `remaining` bytes sent.
///
/// The fetches, decoded and concatenated, are the file from the first
/// planned term's first chunk ([`openxet_cas::Plan`]). At most one stream
/// block (`block`), one stored chunk (`carry`, a chunk split across
/// blocks) and one decoded chunk are held at a time, besides the fetches.
pub(super) struct Transfer {
    fetches: std::vec::IntoIter<Fetch>,
    open: Opener,
    blocks: Option<Blocks>,
    block: Bytes,
    carry: Vec<u8>,
    skip: u64,
    remaining: u64,
    file: String,
    _reserved: Option<Reservation>,
}

impl Transfer {
    /// A read of `remaining` bytes after `skip` of `fetches`, holding
    /// `reserved` until it ends or is dropped. `file` names it in logs.
    pub(super) fn new(
        fetches: Vec<Fetch>,
        open: Opener,
        skip: u64,
        remaining: u64,
        file: String,
        reserved: Option<Reservation>,
    ) -> Self {
        Self {
            fetches: fetches.into_iter(),
            open,
            blocks: None,
            block: Bytes::new(),
            carry: Vec::new(),
            skip,
            remaining,
            file,
            _reserved: reserved,
        }
    }

    /// Opens the next fetch's stream.
    pub(super) async fn open_next(&mut self) -> Result<(), TransferError> {
        let fetch = self.fetches.next().ok_or(TransferError::ShortPlan)?;
        self.blocks = Some((self.open)(fetch).await?);
        Ok(())
    }

    /// The next piece of the range, `None` at its end.
    pub(super) async fn piece(&mut self) -> Result<Option<Bytes>, TransferError> {
        loop {
            if self.remaining == 0 {
                return Ok(None);
            }
            if let Some(stored) = self.take_chunk()? {
                let data = decode_stored_chunk(&stored)?;
                let len = data.len() as u64;
                if self.skip >= len {
                    self.skip -= len;
                    continue;
                }
                let start = self.skip;
                let end = len.min(start + self.remaining);
                self.skip = 0;
                self.remaining -= end - start;
                // Both lie within one chunk, at most `MAX_CHUNK_BYTES`.
                #[allow(clippy::cast_possible_truncation, reason = "within one chunk")]
                return Ok(Some(Bytes::from(data).slice(start as usize..end as usize)));
            }
            let Some(blocks) = self.blocks.as_mut() else {
                self.open_next().await?;
                continue;
            };
            match blocks.next().await {
                Some(block) => self.block = block?,
                None if self.block.is_empty() && self.carry.is_empty() => self.blocks = None,
                None => return Err(TransferError::Truncated),
            }
        }
    }

    /// The next whole stored chunk, if the buffered bytes hold one: sliced
    /// from the block when it lies inside it, else assembled in `carry`.
    fn take_chunk(&mut self) -> Result<Option<Bytes>, TransferError> {
        if self.carry.is_empty() && self.block.len() >= CHUNK_HEADER_BYTES {
            let len = stored_chunk_len(head(&self.block))?;
            if self.block.len() >= len {
                return Ok(Some(self.block.split_to(len)));
            }
        }
        if self.carry.len() < CHUNK_HEADER_BYTES {
            let take = (CHUNK_HEADER_BYTES - self.carry.len()).min(self.block.len());
            self.carry.extend_from_slice(&self.block.split_to(take));
            if self.carry.len() < CHUNK_HEADER_BYTES {
                return Ok(None);
            }
        }
        let len = stored_chunk_len(head(&self.carry))?;
        let take = (len - self.carry.len()).min(self.block.len());
        self.carry.extend_from_slice(&self.block.split_to(take));
        if self.carry.len() < len {
            return Ok(None);
        }
        Ok(Some(Bytes::from(std::mem::take(&mut self.carry))))
    }
}

/// The chunk header `bytes` starts with; the caller checked its length.
fn head(bytes: &[u8]) -> [u8; CHUNK_HEADER_BYTES] {
    let mut header = [0; CHUNK_HEADER_BYTES];
    header.copy_from_slice(&bytes[..CHUNK_HEADER_BYTES]);
    header
}

/// A body's transfer and what the idle timer reads.
struct Shared {
    /// The transfer while it is between pieces; `None` while the stream
    /// works on one, and once it has ended or expired.
    slot: Option<Transfer>,
    /// The body has ended: sent, failed or expired.
    ended: bool,
    /// The idle timer fired: the transfer is dropped, and the next poll
    /// fails the body.
    expired: bool,
    /// When the last piece was taken, or the body made.
    last: Instant,
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The stream's side of a body: the shared state, and the timer, aborted
/// when the body is dropped.
struct Sending {
    shared: Arc<Mutex<Shared>>,
    timer: JoinHandle<()>,
}

impl Drop for Sending {
    fn drop(&mut self) {
        self.timer.abort();
    }
}

/// `transfer` as a response body. The transfer, its reservation, stream
/// and block with it, is dropped when the body ends, is dropped, or goes
/// `idle` without a piece being taken: a client reading slowly but
/// steadily always finishes, and one that stops reading holds nothing
/// past `idle` (ruling M1-R40). The timer drops the transfer even when
/// the server no longer polls the body, as it doesn't while the client's
/// socket is full; the next poll then fails the body, which closes the
/// connection.
///
/// The stream takes the transfer out of the shared state while it works on
/// a piece, so the timer never waits on a bucket read; a read that hangs
/// holds the transfer until the bucket's own timeout, and the body then
/// fails.
pub(super) fn body(transfer: Transfer, idle: Duration) -> Body {
    let shared = Arc::new(Mutex::new(Shared {
        slot: Some(transfer),
        ended: false,
        expired: false,
        last: Instant::now(),
    }));
    let timer = tokio::spawn(expire(Arc::downgrade(&shared), idle));
    let sending = Sending { shared, timer };
    let stream = futures_util::stream::unfold(sending, |sending| async move {
        let taken = {
            let mut state = lock(&sending.shared);
            if state.expired && !state.ended {
                state.ended = true;
                drop(state);
                let e = std::io::Error::other("the gateway body went idle past its timeout");
                return Some((Err(e), sending));
            }
            state.slot.take()
        };
        let mut transfer = taken?;
        let piece = transfer.piece().await;
        let mut state = lock(&sending.shared);
        if state.expired {
            state.ended = true;
            drop(state);
            let e = std::io::Error::other("the gateway body went idle past its timeout");
            return Some((Err(e), sending));
        }
        let item = match piece {
            Ok(Some(piece)) => {
                state.last = Instant::now();
                state.slot = Some(transfer);
                Ok(piece)
            }
            Ok(None) => {
                state.ended = true;
                return None;
            }
            Err(e) => {
                tracing::error!(
                    file = %transfer.file,
                    error = &e as &dyn std::error::Error,
                    "a gateway transfer failed partway"
                );
                state.ended = true;
                Err(std::io::Error::other(e))
            }
        };
        drop(state);
        Some((item, sending))
    });
    Body::from_stream(stream)
}

/// Expires the body once it has gone `idle` without a piece taken.
async fn expire(shared: Weak<Mutex<Shared>>, idle: Duration) {
    loop {
        let Some(shared) = shared.upgrade() else {
            return;
        };
        let wake = {
            let mut state = lock(&shared);
            if state.ended {
                return;
            }
            let wake = state.last + idle;
            if Instant::now() >= wake {
                state.expired = true;
                // Dropped outside the lock.
                let transfer = state.slot.take();
                drop(state);
                drop(transfer);
                tracing::debug!("a gateway body went idle; its memory is released");
                return;
            }
            wake
        };
        drop(shared);
        tokio::time::sleep_until(wake).await;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use openxet_protocol::chunk_hash;

    use super::*;

    /// One uncompressed stored chunk of `data`.
    fn chunk(data: &[u8]) -> Vec<u8> {
        let len = u32::try_from(data.len()).unwrap().to_le_bytes();
        let mut out = vec![0, len[0], len[1], len[2], 0, len[0], len[1], len[2]];
        out.extend_from_slice(data);
        out
    }

    fn fetch(n: u8) -> Fetch {
        Fetch {
            xorb: chunk_hash(&[n]),
            chunks: (0, 1),
            bytes: (0, 0),
        }
    }

    /// An opener yielding `stored[i]` for the `i`-th fetch, in blocks of
    /// `block` bytes.
    fn opener(stored: Vec<Vec<u8>>, block: usize) -> Opener {
        let mut stored = stored.into_iter();
        Box::new(move |_| {
            let bytes = stored.next().unwrap_or_default();
            let blocks: Vec<Result<Bytes, StorageError>> = bytes
                .chunks(block)
                .map(|b| Ok(Bytes::copy_from_slice(b)))
                .collect();
            Box::pin(async move { Ok(futures_util::stream::iter(blocks).boxed()) })
        })
    }

    async fn read_all(mut t: Transfer) -> Result<Vec<u8>, TransferError> {
        let mut out = Vec::new();
        while let Some(piece) = t.piece().await? {
            out.extend_from_slice(&piece);
        }
        Ok(out)
    }

    #[tokio::test]
    async fn chunks_split_anywhere_across_blocks_decode() {
        // Blocks of 1, 3 and 7 bytes split every chunk header and payload,
        // which 1 MiB blocks do only rarely.
        let data: Vec<Vec<u8>> = (0..4u8)
            .map(|i| vec![i + 1; 100 + usize::from(i) * 37])
            .collect();
        let first: Vec<u8> = chunk(&data[0]).into_iter().chain(chunk(&data[1])).collect();
        let second: Vec<u8> = chunk(&data[2]).into_iter().chain(chunk(&data[3])).collect();
        let file: Vec<u8> = data.concat();
        for block in [1, 3, 7, 1 << 20] {
            for (skip, len) in [(0, file.len()), (5, 200), (99, 1), (150, file.len() - 150)] {
                let t = Transfer::new(
                    vec![fetch(0), fetch(1)],
                    opener(vec![first.clone(), second.clone()], block),
                    skip as u64,
                    len as u64,
                    "f".to_owned(),
                    None,
                );
                let got = read_all(t).await.unwrap();
                assert_eq!(got, file[skip..skip + len], "block {block}, {skip}+{len}");
            }
        }
    }

    #[tokio::test]
    async fn a_short_stream_or_plan_fails() {
        let stored = chunk(&[7; 50]);
        let truncated = Transfer::new(
            vec![fetch(0)],
            opener(vec![stored[..30].to_vec()], 7),
            0,
            50,
            "f".to_owned(),
            None,
        );
        assert!(matches!(
            read_all(truncated).await,
            Err(TransferError::Truncated)
        ));
        let short = Transfer::new(
            vec![fetch(0)],
            opener(vec![stored], 7),
            0,
            51,
            "f".to_owned(),
            None,
        );
        assert!(matches!(
            read_all(short).await,
            Err(TransferError::ShortPlan)
        ));
    }

    /// A body over `n` chunks of 1,000 bytes, holding a reservation from
    /// `budget`, going idle after `idle`.
    async fn sent(budget: &openxet_cas::MemoryBudget, n: u8, idle: Duration) -> Body {
        let data: Vec<u8> = (0..n).flat_map(|i| chunk(&[i; 1000])).collect();
        let t = Transfer::new(
            vec![fetch(0)],
            opener(vec![data], 1 << 20),
            0,
            u64::from(n) * 1000,
            "f".to_owned(),
            Some(budget.reserve_answer(4096).await.unwrap()),
        );
        body(t, idle)
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_steady_reader_finishes() {
        // Each piece comes 0.6 s after the last, 12 s in all, far past one
        // idle timeout of 1 s: the body never goes idle.
        use http_body_util::BodyExt as _;
        let budget = openxet_cas::MemoryBudget::new(1 << 20);
        let mut body = sent(&budget, 20, Duration::from_secs(1)).await;
        let mut got = 0;
        while let Some(frame) = body.frame().await {
            got += frame.unwrap().into_data().unwrap().len();
            tokio::time::sleep(Duration::from_millis(600)).await;
        }
        assert_eq!(got, 20_000);
        assert_eq!(budget.available(), budget.bytes());
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_reader_is_cut_off_and_releases() {
        use http_body_util::BodyExt as _;
        let budget = openxet_cas::MemoryBudget::new(1 << 20);
        let mut body = sent(&budget, 20, Duration::from_secs(1)).await;
        body.frame().await.unwrap().unwrap();
        assert!(budget.available() < budget.bytes());
        tokio::time::sleep(Duration::from_millis(999)).await;
        assert!(budget.available() < budget.bytes(), "not idle yet");
        tokio::time::sleep(Duration::from_millis(2)).await;
        assert_eq!(budget.available(), budget.bytes(), "released when idle");
        assert!(body.frame().await.unwrap().is_err(), "then the body fails");
        assert!(body.frame().await.is_none());
    }
}
