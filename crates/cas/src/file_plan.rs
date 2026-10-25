//! [`plan_file`]: a file range's reconstruction plan, for the parts of the
//! server that stream a file rather than answer a reconstruction, such as
//! the Hub's file gateway (ruling M2-R9).
//!
//! It plans exactly as the reconstruction routes do ([`crate::plan`]), from
//! the same index reads, and signs nothing. A file is planned only for the
//! scope it was registered to (design §5.4, ruling M1-R16), so a file of
//! another scope reads as never stored.

use std::mem::size_of;

use openxet_index::{Index, IndexError, Visibility};
use openxet_protocol::{MerkleHash, hash_to_hex};

use crate::CasError;
use crate::budget::{MemoryBudget, Reservation};
use crate::plan::{self, Fetch, Plan, PlanError, PlannedTerm, XorbMap};
use crate::reconstruction::{ByteRange, DECODED_TERM_BYTES, distinct_xorbs, resolve};

/// The memory one planned term takes: its [`PlannedTerm`], and at most one
/// [`Fetch`].
const PLANNED_TERM_BYTES: u64 = (size_of::<PlannedTerm>() + size_of::<Fetch>()) as u64;

/// A file's plan for a range, what an HTTP answer needs besides, and the
/// memory reserved for the plan.
#[derive(Debug)]
pub struct FilePlan {
    /// The file's size in bytes.
    pub size: u64,
    /// The bytes the plan yields, inclusive, the end clamped to the file;
    /// `None` for the whole file, the empty one included.
    pub range: Option<(u64, u64)>,
    /// The terms and fetches that yield them.
    pub plan: Plan,
    /// The plan's bytes, reserved from the budget (ruling M1-R40): the
    /// caller holds it as long as it holds the plan, and shrinks it to what
    /// it keeps.
    pub reserved: Reservation,
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
    /// The memory budget refused the plan: [`CasError::MemoryBusy`] after
    /// its wait, or [`CasError::AnswerTooLarge`] for a plan larger than one
    /// answer's share.
    #[error(transparent)]
    Memory(CasError),
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
/// to `scope`, with its memory reserved from `budget`.
///
/// It reserves as the reconstruction routes do (ruling M1-R40): the
/// decoded record by its stored term count before loading it, then the
/// plan's bound once the range is resolved to terms, both checked against
/// one answer's share. It then loads the file's record and term byte
/// ranges, and the chunk layouts of only the xorbs whose terms the range
/// trims (all the window's xorbs for a file registered without byte
/// ranges). Once planned, the record is dropped and the reservation shrunk
/// to the plan.
///
/// Planning runs on the calling task, without a `validate` permit: unlike
/// a reconstruction answer it signs and serializes nothing, and it is
/// linear in the window's terms, about the cost of decoding the record.
///
/// # Errors
///
/// [`FilePlanError::NotFound`] for a file not registered to `scope`,
/// [`FilePlanError::Unsatisfiable`] for a range past its end,
/// [`FilePlanError::Memory`] when the budget refuses,
/// [`FilePlanError::Inconsistent`] for a record that disagrees with its
/// layouts, and the index's failures.
pub async fn plan_file(
    index: &dyn Index,
    budget: &MemoryBudget,
    scope: &str,
    file: &MerkleHash,
    range: Option<ByteRange>,
) -> Result<FilePlan, FilePlanError> {
    let stored = index.term_count(&[*file], scope).await?;
    let decoded = stored.saturating_mul(DECODED_TERM_BYTES);
    let mut reserved = budget
        .reserve_answer(decoded)
        .await
        .map_err(FilePlanError::Memory)?;
    let (record, bytes) = index
        .file_with_bytes(file, scope)
        .await?
        .ok_or(FilePlanError::NotFound)?;
    let size = record.size;
    // `resolve` refuses only unsatisfiable suffixes.
    let range = resolve(range, size).map_err(|_| FilePlanError::Unsatisfiable { size })?;
    let window = plan::window(&record, range).map_err(planning_failed)?;
    let terms = window.as_ref().map_or(0, |w| w.terms.len() as u64);
    let bound = terms.saturating_mul(PLANNED_TERM_BYTES);
    budget
        .check_answer(decoded.saturating_add(bound))
        .map_err(FilePlanError::Memory)?;
    reserved.merge(
        budget
            .reserve_answer(bound)
            .await
            .map_err(FilePlanError::Memory)?,
    );
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
            "file {}: names xorb {}, whose chunk layout the index doesn't hold",
            hash_to_hex(file),
            hash_to_hex(missing)
        )));
    }
    let tables: XorbMap<_> = tables.into_iter().collect();
    let plan = plan::plan(&record, bytes.as_ref(), &tables, range).map_err(planning_failed)?;
    drop((record, bytes, tables));
    reserved.shrink_to(plan_bytes(&plan));
    // A satisfiable range starts inside the file, so `size > 0` here.
    let range = range.map(|(start, end)| (start, end.min(size - 1)));
    Ok(FilePlan {
        size,
        range,
        plan,
        reserved,
    })
}

/// The bytes `plan`'s vectors hold.
fn plan_bytes(plan: &Plan) -> u64 {
    (plan.terms.capacity() * size_of::<PlannedTerm>()
        + plan.fetches.capacity() * size_of::<Fetch>()) as u64
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
        let (index, budget) = (state.index.as_ref(), &state.budget);
        let plan = |scope: &'static str, range| plan_file(index, budget, scope, &hash, range);
        let shape = |p: &FilePlan| (p.size, p.range, p.plan.clone());

        let whole = plan("s", None).await.unwrap();
        assert_eq!((whole.size, whole.range), (40, None));
        assert_eq!(whole.plan.fetches.len(), 1, "adjacent terms share a fetch");
        assert_eq!(whole.plan.fetches[0].bytes, (0, 71));

        let from = |start, end| Some(ByteRange::From { start, end });
        let middle = plan("s", from(15, Some(24))).await.unwrap();
        assert_eq!(middle.range, Some((15, 24)));
        assert_eq!(middle.plan.offset_into_first_range, 5);
        assert_eq!(middle.plan.fetches[0].chunks, (1, 3));
        assert_eq!(middle.plan.fetches[0].bytes, (18, 53));

        let tail = plan("s", from(35, None)).await.unwrap();
        assert_eq!(tail.range, Some((35, 39)));
        let suffix = plan("s", Some(ByteRange::Suffix(5))).await.unwrap();
        assert_eq!(shape(&suffix), shape(&tail));
        let clamped = plan("s", from(35, Some(1000))).await.unwrap();
        assert_eq!(shape(&clamped), shape(&tail));

        for range in [
            from(40, None),
            from(41, Some(50)),
            Some(ByteRange::Suffix(0)),
        ] {
            assert!(matches!(
                plan("s", range).await,
                Err(FilePlanError::Unsatisfiable { size: 40 })
            ));
        }
        assert!(matches!(
            plan("other", None).await,
            Err(FilePlanError::NotFound)
        ));
    }

    #[tokio::test]
    async fn a_plan_holds_its_reservation_until_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let (state, hash) = file(dir.path()).await;
        let budget = &state.budget;
        let full = budget.available();
        let planned = plan_file(state.index.as_ref(), budget, "s", &hash, None)
            .await
            .unwrap();
        assert!(budget.available() < full, "the plan is reserved");
        drop(planned);
        assert_eq!(budget.available(), full);

        // A budget whose answer share can't hold the decoded record.
        let tiny = MemoryBudget::new(1024);
        assert!(matches!(
            plan_file(state.index.as_ref(), &tiny, "s", &hash, None).await,
            Err(FilePlanError::Memory(CasError::AnswerTooLarge { .. }))
        ));
        assert_eq!(tiny.available(), tiny.bytes());
    }
}
