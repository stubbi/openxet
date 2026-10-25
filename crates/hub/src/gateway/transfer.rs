//! A file range read for the gateway: the plan's fetches in order, each
//! streamed from the bucket and decoded one chunk at a time (ruling
//! M1-R27), sent as a response body that holds the plan's memory
//! reservation until it is sent, dropped, or overruns its send deadline
//! (ruling M1-R40).

use std::sync::{Arc, Weak};
use std::time::Duration;

use axum::body::{Body, Bytes};
use futures_util::StreamExt as _;
use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use openxet_cas::{Fetch, Reservation};
use openxet_protocol::{CHUNK_HEADER_BYTES, ProtocolError, decode_stored_chunk, stored_chunk_len};
use openxet_storage::{Bucket, Key, StorageError};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::HubError;

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

/// Where a body's transfer is.
enum State {
    Sending(Transfer),
    Done,
    Expired,
}

/// The body's state and the timer that expires it.
struct Sending {
    state: Arc<Mutex<State>>,
    timer: JoinHandle<()>,
}

impl Drop for Sending {
    fn drop(&mut self) {
        self.timer.abort();
    }
}

/// `transfer` as a response body. It is dropped, its reservation, stream
/// and block with it, when the body ends, is dropped, or `deadline`
/// passes: a timer drops it even if the server never polls the body again,
/// as it doesn't while the client's socket is full, and the next poll then
/// fails the body, which closes the connection (rulings M1-R33, M1-R40).
pub(super) fn body(transfer: Transfer, deadline: Duration) -> Body {
    let state = Arc::new(Mutex::new(State::Sending(transfer)));
    let timer = tokio::spawn(expire(Arc::downgrade(&state), deadline));
    let sending = Sending { state, timer };
    let stream = futures_util::stream::unfold(sending, |sending| async move {
        let item = {
            let mut state = sending.state.lock().await;
            match &mut *state {
                State::Sending(transfer) => match transfer.piece().await {
                    Ok(Some(piece)) => Some(Ok(piece)),
                    Ok(None) => {
                        *state = State::Done;
                        None
                    }
                    Err(e) => {
                        tracing::error!(
                            file = %transfer.file,
                            error = &e as &dyn std::error::Error,
                            "a gateway transfer failed partway"
                        );
                        *state = State::Done;
                        Some(Err(std::io::Error::other(e)))
                    }
                },
                State::Done => None,
                State::Expired => {
                    *state = State::Done;
                    Some(Err(std::io::Error::other("the send deadline passed")))
                }
            }
        };
        item.map(|item| (item, sending))
    });
    Body::from_stream(stream)
}

async fn expire(state: Weak<Mutex<State>>, deadline: Duration) {
    tokio::time::sleep(deadline).await;
    if let Some(state) = state.upgrade() {
        let mut state = state.lock().await;
        if matches!(*state, State::Sending(_)) {
            *state = State::Expired;
            tracing::debug!("a gateway body missed its send deadline; its memory is released");
        }
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
}
