//! The interposer's read side (ADR 0004 item 2): a request body is read up
//! to the end of the part openxet checks, its first flush packet (the
//! whole command request in protocol v2, the want section in v0/v1),
//! before anything reaches git. The rest of the body stays unread in its
//! stream.

use futures_util::StreamExt as _;
use openxet_storage::BodyStream;

use crate::pkt::{PktError, find_flush};

/// The most bytes openxet buffers before a request's first flush: about
/// 340,000 `want` or `have` lines, past any stock client's request.
pub(crate) const MAX_HEAD_BYTES: usize = 16 * 1024 * 1024;

/// A request split at the end of its first flush packet.
pub(crate) struct Split {
    /// The bytes through the flush.
    pub(crate) head: Vec<u8>,
    /// The bytes already read past it.
    pub(crate) extra: Vec<u8>,
    /// The body's unread remainder.
    pub(crate) rest: BodyStream,
}

/// Why the head couldn't be read.
#[derive(Debug, thiserror::Error)]
pub(crate) enum HeadError {
    /// The client sent a malformed request: answered with `ERR`.
    #[error(transparent)]
    Pkt(#[from] PktError),
    /// No flush within [`MAX_HEAD_BYTES`]: answered with `ERR`.
    #[error(
        "protocol error: the request sends more than 16 MiB before its first flush packet; fetch \
         fewer refs at once"
    )]
    TooLarge,
    /// Reading the body failed.
    #[error(transparent)]
    Io(std::io::Error),
}

/// Reads `body` up to the end of its first flush packet.
pub(crate) async fn read_head(mut body: BodyStream) -> Result<Split, HeadError> {
    let mut buf = Vec::new();
    let mut scanned = 0;
    loop {
        match find_flush(&buf, scanned)? {
            Ok(end) => {
                let extra = buf.split_off(end);
                return Ok(Split {
                    head: buf,
                    extra,
                    rest: body,
                });
            }
            Err(resume) => scanned = resume,
        }
        if buf.len() >= MAX_HEAD_BYTES {
            return Err(HeadError::TooLarge);
        }
        match body.next().await {
            Some(Ok(chunk)) => buf.extend_from_slice(&chunk),
            Some(Err(e)) => return Err(HeadError::Io(e)),
            None => return Err(HeadError::Pkt(PktError::Truncated)),
        }
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use futures_util::StreamExt as _;
    use openxet_storage::BodyStream;

    use super::{HeadError, MAX_HEAD_BYTES, read_head};
    use crate::pkt::PktError;

    fn body(chunks: Vec<Vec<u8>>) -> BodyStream {
        futures_util::stream::iter(chunks.into_iter().map(|c| Ok(Bytes::from(c)))).boxed()
    }

    #[tokio::test]
    async fn split_at_first_flush_byte_by_byte() {
        let request = b"0009want\n00000009have\n0009done\n".to_vec();
        let chunks = request.iter().map(|b| vec![*b]).collect();
        let Ok(mut split) = read_head(body(chunks)).await else {
            panic!("read_head failed");
        };
        assert_eq!(split.head, b"0009want\n0000");
        let mut rest = split.extra.clone();
        while let Some(Ok(chunk)) = split.rest.next().await {
            rest.extend_from_slice(&chunk);
        }
        assert_eq!(rest, b"0009have\n0009done\n");
    }

    #[tokio::test]
    async fn truncated_and_oversized_heads_are_errors() {
        let Err(HeadError::Pkt(PktError::Truncated)) =
            read_head(body(vec![b"0009want\n".to_vec()])).await
        else {
            panic!("a request without a flush was accepted");
        };
        let mut line = Vec::new();
        crate::pkt::write_pkt(&mut line, &vec![b'x'; 60000]);
        let chunks = vec![line; MAX_HEAD_BYTES / 60000 + 2];
        let Err(HeadError::TooLarge) = read_head(body(chunks)).await else {
            panic!("an oversized head was accepted");
        };
    }
}
