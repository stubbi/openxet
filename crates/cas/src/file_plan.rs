//! [`plan_file`]: a file range's reconstruction plan, for the parts of the
//! server that stream a file rather than answer a reconstruction, such as
//! the Hub's file gateway (ruling M2-R9).
//!
//! It plans exactly as the reconstruction routes do ([`crate::plan`]), from
//! the same index reads, and signs nothing. A file is planned only for the
//! scope it was registered to (design §5.4, ruling M1-R16), so a file of
//! another scope reads as never stored.

use openxet_index::{Index, IndexError, Visibility};
use openxet_protocol::{MerkleHash, hash_to_hex};

use crate::plan::{self, Plan, PlanError, XorbMap};
use crate::reconstruction::{ByteRange, distinct_xorbs, resolve};

/// A file's plan for a range, and what an HTTP answer needs besides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePlan {
    /// The file's size in bytes.
    pub size: u64,
    /// The bytes the plan yields, inclusive, the end clamped to the file;
    /// `None` for the whole file, the empty one included.
    pub range: Option<(u64, u64)>,
    /// The terms and fetches that yield them.
    pub plan: Plan,
}

/// Why a file range has no plan.
#[derive(Debug, thiserror::Error)]
pub enum FilePlanError {
    /// The file isn't registered to the scope.
    #[error("the file is not registered to this scope")]
    NotFound,
    /// The range starts at or past the end of the file, which is `size`
    /// bytes, or is an empty suffix: `416` (RFC 9110 §15.5.17).
    #[error("the range is not satisfiable for a file of {size} bytes")]
    Unsatisfiable {
        /// The file's size.
        size: u64,
    },
    /// The file's record disagrees with itself or with a chunk layout,
    /// which registration checked (ruling M1-R17): index corruption, never
    /// the caller's fault.
    #[error("integrity anomaly: {0}")]
    Inconsistent(String),
    /// The index failed.
    #[error(transparent)]
    Index(#[from] IndexError),
}

/// The plan of `range` (the whole file for `None`) of `file` as registered
/// to `scope`.
///
/// It loads the file's record and term byte ranges, then the chunk layouts
/// of only the xorbs whose terms the range trims (all the window's xorbs
/// for a file registered without byte ranges), as the reconstruction
/// routes do. Its memory is the decoded record and the plan: about 100
/// bytes a term.
///
/// # Errors
///
/// [`FilePlanError::NotFound`] for a file not registered to `scope`,
/// [`FilePlanError::Unsatisfiable`] for a range past its end,
/// [`FilePlanError::Inconsistent`] for a record that disagrees with its
/// layouts, and the index's failures.
pub async fn plan_file(
    index: &dyn Index,
    scope: &str,
    file: &MerkleHash,
    range: Option<ByteRange>,
) -> Result<FilePlan, FilePlanError> {
    let (record, bytes) = index
        .file_with_bytes(file, scope)
        .await?
        .ok_or(FilePlanError::NotFound)?;
    let size = record.size;
    // `resolve` refuses only unsatisfiable suffixes.
    let range = resolve(range, size).map_err(|_| FilePlanError::Unsatisfiable { size })?;
    let window = plan::window(&record, range).map_err(planning_failed)?;
    let xorbs = match (&window, &bytes) {
        (None, _) => Vec::new(),
        (Some(w), Some(_)) => w
            .trimmed(&record)
            .iter()
            .map(|&t| record.terms[t].xorb)
            .collect(),
        (Some(w), None) => distinct_xorbs(&record.terms[w.terms.clone()]),
    };
    // Reads follow the scope, not the tenant (ruling M1-R38): `Global`
    // names no tenant.
    let tables = index.chunk_layouts(&xorbs, "", Visibility::Global).await?;
    if let Some(missing) = xorbs.iter().find(|x| !tables.contains_key(x)) {
        return Err(FilePlanError::Inconsistent(format!(
            "file {} names xorb {}, whose chunk layout the index doesn't hold",
            hash_to_hex(file),
            hash_to_hex(missing)
        )));
    }
    let tables: XorbMap<_> = tables.into_iter().collect();
    let plan = plan::plan(&record, bytes.as_ref(), &tables, range).map_err(planning_failed)?;
    // A satisfiable range starts inside the file, so `size > 0` here.
    let range = range.map(|(start, end)| (start, end.min(size - 1)));
    Ok(FilePlan { size, range, plan })
}

fn planning_failed(e: PlanError) -> FilePlanError {
    match e {
        PlanError::Unsatisfiable { size } => FilePlanError::Unsatisfiable { size },
        PlanError::Inconsistent(detail) => FilePlanError::Inconsistent(detail),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use openxet_index::{ChunkTable, FileRecord, NewFile, Registrant, Term};
    use openxet_protocol::chunk_hash;

    use super::*;
    use crate::test_support;

    /// An index holding a file of two terms over one xorb of four 10-byte
    /// chunks (stored 18 bytes each), registered to `s`.
    async fn file(dir: &std::path::Path) -> (crate::AppState, MerkleHash) {
        let state = test_support::state(dir, vec![]).await;
        let xorb = chunk_hash(b"xorb");
        let table = ChunkTable {
            hashes: (0..4u8).map(|c| (chunk_hash(&[c]), 10)).collect(),
            offsets: (0..=4u32).map(|c| c * 18).collect(),
        };
        state
            .index
            .put_xorb(&xorb, 72, &table, &[], "s")
            .await
            .unwrap();
        let term = |chunk_start, chunk_end| Term {
            xorb,
            chunk_start,
            chunk_end,
            unpacked_bytes: (chunk_end - chunk_start) * 10,
        };
        let record = FileRecord {
            hash: chunk_hash(b"file"),
            size: 40,
            terms: vec![term(0, 2), term(2, 4)],
        };
        let hash = record.hash;
        let new = NewFile {
            record,
            sha256: None,
            first_chunk: None,
        };
        let by = Registrant {
            scope: "s",
            tenant: "s",
            seq: 1,
        };
        state
            .index
            .register_shard(&xorb, &[new], by, Visibility::Global)
            .await
            .unwrap();
        (state, hash)
    }

    #[tokio::test]
    async fn a_file_plans_whole_and_by_range_in_its_scope_only() {
        let dir = tempfile::tempdir().unwrap();
        let (state, hash) = file(dir.path()).await;
        let index = state.index.as_ref();

        let whole = plan_file(index, "s", &hash, None).await.unwrap();
        assert_eq!((whole.size, whole.range), (40, None));
        assert_eq!(whole.plan.fetches.len(), 1, "adjacent terms share a fetch");
        assert_eq!(whole.plan.fetches[0].bytes, (0, 71));

        let from = |start, end| Some(ByteRange::From { start, end });
        let middle = plan_file(index, "s", &hash, from(15, Some(24)))
            .await
            .unwrap();
        assert_eq!(middle.range, Some((15, 24)));
        assert_eq!(middle.plan.offset_into_first_range, 5);
        assert_eq!(middle.plan.fetches[0].chunks, (1, 3));
        assert_eq!(middle.plan.fetches[0].bytes, (18, 53));

        let tail = plan_file(index, "s", &hash, from(35, None)).await.unwrap();
        assert_eq!(tail.range, Some((35, 39)));
        let suffix = plan_file(index, "s", &hash, Some(ByteRange::Suffix(5)))
            .await
            .unwrap();
        assert_eq!(suffix, tail);
        let clamped = plan_file(index, "s", &hash, from(35, Some(1000)))
            .await
            .unwrap();
        assert_eq!(clamped, tail);

        for range in [
            from(40, None),
            from(41, Some(50)),
            Some(ByteRange::Suffix(0)),
        ] {
            assert!(matches!(
                plan_file(index, "s", &hash, range).await,
                Err(FilePlanError::Unsatisfiable { size: 40 })
            ));
        }
        assert!(matches!(
            plan_file(index, "other", &hash, None).await,
            Err(FilePlanError::NotFound)
        ));
    }
}
