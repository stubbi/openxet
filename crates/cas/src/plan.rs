//! Reconstruction planning: the terms, chunk ranges and stored byte ranges
//! that answer a range of a file (S10 §1, "Reconstruction"; ADR 0002).
//!
//! Planning is pure, with no I/O and no signing, so it can be checked
//! exhaustively; the handler signs one URL per [`Fetch`].
//!
//! Terms are trimmed to whole chunks, as upstream's reference server does
//! (`xet-client` 1.7.0, `simulation/xorb_utils.rs`): the first kept term
//! starts at the chunk holding the range's first byte, and the last ends
//! after the chunk holding its last byte. The client skips
//! `offset_into_first_range` bytes into the first term and trims after the
//! last (S10 §1).

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use foldhash::fast::RandomState;
use openxet_index::{ChunkLayout, FileRecord, Term, TermBytes};
use openxet_protocol::{MerkleHash, hash_to_hex};

/// A map keyed by xorb hash, for the per-request maps of a reconstruction,
/// which a 100k-term answer queries several times per term: with `SipHash`
/// they took about 10 ms of it. foldhash is no defence against an attacker
/// who can both choose keys and watch one map's timings, but these keys are
/// BLAKE3 outputs no caller can choose without a preimage search, and each
/// map lives for one request under its own random seed, so no key list
/// collides in every map.
pub(crate) type XorbMap<V> = HashMap<MerkleHash, V, RandomState>;

/// A set of xorb hashes, as [`XorbMap`].
pub(crate) type XorbSet = HashSet<MerkleHash, RandomState>;

/// What answers a file range: the terms in file order and one fetch
/// descriptor per stored byte range, before any URL is signed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Plan {
    /// The bytes to skip in the first term: where the range starts inside
    /// that term's first chunk.
    pub(crate) offset_into_first_range: u64,
    /// The terms, in file order.
    pub(crate) terms: Vec<PlannedTerm>,
    /// The descriptors, in the order of the terms that first use them.
    /// Every term lies inside one descriptor of its xorb.
    pub(crate) fetches: Vec<Fetch>,
}

/// One term of a plan: chunks `chunks.0..chunks.1` of `xorb`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlannedTerm {
    /// The xorb holding the chunks.
    pub(crate) xorb: MerkleHash,
    /// The chunk range, end-exclusive.
    pub(crate) chunks: (u32, u32),
    /// The sum of the chunks' uncompressed lengths, which the client checks
    /// against what it decodes (S10 §1).
    pub(crate) unpacked_length: u32,
    /// The index in [`Plan::fetches`] of the descriptor holding the term.
    pub(crate) fetch: usize,
}

/// One fetch descriptor: chunks `chunks.0..chunks.1` of `xorb`, stored at
/// bytes `bytes.0..=bytes.1`, exactly their chunk headers and payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fetch {
    /// The xorb to fetch from.
    pub(crate) xorb: MerkleHash,
    /// The chunk range, end-exclusive.
    pub(crate) chunks: (u32, u32),
    /// The byte range in the stored xorb, end-inclusive.
    pub(crate) bytes: (u64, u64),
}

/// Why a file range has no plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlanError {
    /// The range starts at or past the end of the file, which is `size`
    /// bytes. `416` (S10 §1).
    Unsatisfiable {
        /// The file's size.
        size: u64,
    },
    /// The file's record disagrees with itself or with a chunk table. The
    /// index checked both at registration (ruling M1-R17), so this is index
    /// corruption, never the caller's fault.
    Inconsistent(String),
}

/// The part of a file a range keeps: the byte range, clamped to the file,
/// and the terms that overlap it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Window {
    /// The first byte, inclusive.
    start: u64,
    /// The last byte, inclusive, at most the file's last.
    end: u64,
    /// The indices of the terms that overlap `start..=end`.
    pub(crate) terms: Range<usize>,
    /// Where the first of those terms starts in the file.
    first_term_at: u64,
    /// Where the last of those terms ends in the file, exclusive.
    last_term_end: u64,
}

impl Window {
    /// Whether the term starting at `at` and holding `len` bytes lies
    /// wholly inside the window, so a plan keeps all of its chunks.
    fn holds_whole(&self, at: u64, len: u32) -> bool {
        at >= self.start && at + u64::from(len) <= self.end + 1
    }

    /// The indices of the window's terms a plan trims to the range: at
    /// most its first and its last. Only they need their xorbs' layouts
    /// when the file's [`TermBytes`] are known.
    pub(crate) fn trimmed(&self, file: &FileRecord) -> Vec<usize> {
        let (first, last) = (self.terms.start, self.terms.end - 1);
        let mut out = Vec::with_capacity(2);
        if !self.holds_whole(self.first_term_at, file.terms[first].unpacked_bytes) {
            out.push(first);
        }
        let last_len = file.terms[last].unpacked_bytes;
        if last != first && !self.holds_whole(self.last_term_end - u64::from(last_len), last_len) {
            out.push(last);
        }
        out
    }
}

/// The window of `file` that `range` (inclusive, its end clamped to the
/// file) keeps, or `None` for the whole of an empty file. `None` asks for
/// the whole file.
///
/// # Errors
///
/// [`PlanError::Unsatisfiable`] if the range starts at or past the end of
/// the file, or ends before it starts, and [`PlanError::Inconsistent`] if
/// the terms' lengths don't add up to the file's size.
pub(crate) fn window(
    file: &FileRecord,
    range: Option<(u64, u64)>,
) -> Result<Option<Window>, PlanError> {
    let size = file.size;
    // At most 2^32 terms of at most 2^32 bytes each: the sum fits a u64.
    let total: u64 = file.terms.iter().map(|t| u64::from(t.unpacked_bytes)).sum();
    if total != size {
        return Err(PlanError::Inconsistent(format!(
            "file {}: its terms hold {total} bytes, its size is {size}",
            hash_to_hex(&file.hash)
        )));
    }
    let (start, end) = match range {
        None if size == 0 => return Ok(None),
        None => (0, size - 1),
        Some((start, end)) if start >= size || end < start => {
            return Err(PlanError::Unsatisfiable { size });
        }
        Some((start, end)) => (start, end.min(size - 1)),
    };
    let mut at = 0u64;
    let mut first = 0;
    // Terminates before running out: `start < size`, the sum of the terms.
    while at + u64::from(file.terms[first].unpacked_bytes) <= start {
        at += u64::from(file.terms[first].unpacked_bytes);
        first += 1;
    }
    let first_term_at = at;
    let mut last = first;
    while last < file.terms.len() && at <= end {
        at += u64::from(file.terms[last].unpacked_bytes);
        last += 1;
    }
    Ok(Some(Window {
        start,
        end,
        terms: first..last,
        first_term_at,
        last_term_end: at,
    }))
}

/// The plan answering `range` of `file` (see [`window`]).
///
/// With the file's [`TermBytes`], a term the window holds whole is planned
/// from its recorded bytes, and `tables` needs the chunk layouts only of
/// the xorbs of the terms [`Window::trimmed`] names. Without them, `tables`
/// holds the layout of every xorb the window's terms name. Both give the
/// same plan: the bytes were derived from the layouts at registration,
/// which also checked each term against its chunks (ruling M1-R17).
///
/// Consecutive terms of the same xorb whose chunk ranges are adjacent share
/// one descriptor; every other term gets its own. A descriptor never holds
/// more than one range (ADR 0002 §1), and its bytes are
/// `offsets[a]..=offsets[b] - 1` for chunks `a..b`.
///
/// Linear in the window's terms, and in the chunks of the terms planned
/// from their layouts.
///
/// # Errors
///
/// As [`window`], and [`PlanError::Inconsistent`] if a table is missing or
/// disagrees with a term.
pub(crate) fn plan(
    file: &FileRecord,
    bytes: Option<&TermBytes>,
    tables: &XorbMap<ChunkLayout>,
    range: Option<(u64, u64)>,
) -> Result<Plan, PlanError> {
    let Some(window) = window(file, range)? else {
        return Ok(Plan::default());
    };
    let mut out = Plan {
        offset_into_first_range: 0,
        terms: Vec::with_capacity(window.terms.len()),
        // At most one per term.
        fetches: Vec::with_capacity(window.terms.len()),
    };
    let mut term_at = window.first_term_at;
    for (i, term) in file.terms[window.terms.clone()].iter().enumerate() {
        let recorded = bytes
            .filter(|_| window.holds_whole(term_at, term.unpacked_bytes))
            .map(|b| b[window.terms.start + i]);
        // A term held whole has no offset into it either.
        let (kept, unpacked_length, stored) = if let Some((start, end)) = recorded {
            (
                (term.chunk_start, term.chunk_end),
                term.unpacked_bytes,
                (u64::from(start), u64::from(end) - 1),
            )
        } else {
            let trimmed = trim(file, term, tables, &window, term_at, i == 0)?;
            if i == 0 {
                out.offset_into_first_range = trimmed.offset;
            }
            (trimmed.kept, trimmed.unpacked_length, trimmed.bytes)
        };
        match out.fetches.last_mut() {
            // The previous term's descriptor is always the last one.
            Some(last) if last.xorb == term.xorb && last.chunks.1 == kept.0 => {
                last.chunks.1 = kept.1;
                last.bytes.1 = stored.1;
            }
            _ => out.fetches.push(Fetch {
                xorb: term.xorb,
                chunks: kept,
                bytes: stored,
            }),
        }
        out.terms.push(PlannedTerm {
            xorb: term.xorb,
            chunks: kept,
            unpacked_length,
            fetch: out.fetches.len() - 1,
        });
        term_at += u64::from(term.unpacked_bytes);
    }
    Ok(out)
}

/// What a term planned from its xorb's layout keeps.
struct Trimmed {
    /// The chunk range, end-exclusive.
    kept: (u32, u32),
    /// The kept chunks' uncompressed bytes.
    unpacked_length: u32,
    /// Their stored bytes, end-inclusive.
    bytes: (u64, u64),
    /// Where the window starts in the first kept chunk; meaningful for the
    /// window's first term only.
    offset: u64,
}

/// `term`, starting at `term_at` in the file, trimmed to `window` from its
/// xorb's layout: from the chunk holding the window's first byte if it is
/// the window's first term, to the chunk holding its last byte.
fn trim(
    file: &FileRecord,
    term: &Term,
    tables: &XorbMap<ChunkLayout>,
    window: &Window,
    term_at: u64,
    first_term: bool,
) -> Result<Trimmed, PlanError> {
    let table = tables.get(&term.xorb).ok_or_else(|| {
        PlanError::Inconsistent(format!(
            "file {}: no chunk table for xorb {}",
            hash_to_hex(&file.hash),
            hash_to_hex(&term.xorb)
        ))
    })?;
    let chunks = term_chunks(file, term, table)?;

    // The first chunk kept, and where it starts in the file.
    let mut first = 0;
    let mut chunk_at = term_at;
    let mut offset = 0;
    if first_term {
        while chunk_at + u64::from(chunks[first]) <= window.start {
            chunk_at += u64::from(chunks[first]);
            first += 1;
            if first == chunks.len() {
                return Err(short_term(file, term));
            }
        }
        offset = window.start - chunk_at;
    }
    // The chunks up to the one holding the window's last byte.
    let mut kept_end = first;
    let mut at = chunk_at;
    while kept_end < chunks.len() && at <= window.end {
        at += u64::from(chunks[kept_end]);
        kept_end += 1;
    }
    if kept_end == chunks.len() && at - term_at != u64::from(term.unpacked_bytes) {
        return Err(short_term(file, term));
    }
    let unpacked_length = u32::try_from(at - chunk_at).map_err(|_| short_term(file, term))?;

    // Chunk indices fit u32: `term_chunks` bounded them by the term's.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "bounded by the term's u32 chunk range"
    )]
    let kept = (
        term.chunk_start + first as u32,
        term.chunk_start + kept_end as u32,
    );
    let bytes = stored_bytes(file, term, table, kept)?;
    Ok(Trimmed {
        kept,
        unpacked_length,
        bytes,
        offset,
    })
}

/// The uncompressed lengths of `term`'s chunks in `table`.
fn term_chunks<'t>(
    file: &FileRecord,
    term: &Term,
    table: &'t ChunkLayout,
) -> Result<&'t [u32], PlanError> {
    let range = term.chunk_start as usize..term.chunk_end as usize;
    match table.lengths.get(range) {
        Some(chunks) if !chunks.is_empty() && table.offsets.len() == table.lengths.len() + 1 => {
            Ok(chunks)
        }
        _ => Err(PlanError::Inconsistent(format!(
            "file {}: chunks {}..{} of xorb {} are not in its table of {} chunks",
            hash_to_hex(&file.hash),
            term.chunk_start,
            term.chunk_end,
            hash_to_hex(&term.xorb),
            table.lengths.len()
        ))),
    }
}

/// The stored bytes of `chunks`, end-inclusive.
fn stored_bytes(
    file: &FileRecord,
    term: &Term,
    table: &ChunkLayout,
    (a, b): (u32, u32),
) -> Result<(u64, u64), PlanError> {
    let start = u64::from(table.offsets[a as usize]);
    let end = u64::from(table.offsets[b as usize]);
    if end <= start {
        return Err(PlanError::Inconsistent(format!(
            "file {}: xorb {} stores chunks {a}..{b} in no bytes",
            hash_to_hex(&file.hash),
            hash_to_hex(&term.xorb)
        )));
    }
    Ok((start, end - 1))
}

fn short_term(file: &FileRecord, term: &Term) -> PlanError {
    PlanError::Inconsistent(format!(
        "file {}: chunks {}..{} of xorb {} don't hold the term's {} bytes",
        hash_to_hex(&file.hash),
        term.chunk_start,
        term.chunk_end,
        hash_to_hex(&term.xorb),
        term.unpacked_bytes
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::cast_possible_truncation)]

    use openxet_protocol::chunk_hash;
    use proptest::prelude::*;

    use super::*;

    /// A xorb of uncompressed chunks: their bytes, its stored form (each
    /// chunk an 8-byte header and its payload) and its table.
    struct TestXorb {
        hash: MerkleHash,
        chunks: Vec<Vec<u8>>,
        stored: Vec<u8>,
        table: ChunkLayout,
    }

    fn test_xorb(index: usize, lengths: &[u32]) -> TestXorb {
        let chunks: Vec<Vec<u8>> = lengths
            .iter()
            .enumerate()
            .map(|(c, &len)| {
                (0..len)
                    .map(|i| (index * 31 + c * 7 + i as usize) as u8)
                    .collect()
            })
            .collect();
        let mut stored = Vec::new();
        let mut offsets = Vec::new();
        for data in &chunks {
            offsets.push(stored.len() as u32);
            let len = (data.len() as u32).to_le_bytes();
            stored.extend_from_slice(&[0, len[0], len[1], len[2], 0, len[0], len[1], len[2]]);
            stored.extend_from_slice(data);
        }
        offsets.push(stored.len() as u32);
        TestXorb {
            hash: chunk_hash(&[index as u8]),
            table: ChunkLayout {
                lengths: chunks.iter().map(|d| d.len() as u32).collect(),
                offsets: offsets.into(),
            },
            chunks,
            stored,
        }
    }

    /// The payloads of the chunks stored in `bytes`, read header by header.
    fn decode(bytes: &[u8]) -> Vec<&[u8]> {
        let mut out = Vec::new();
        let mut rest = bytes;
        while !rest.is_empty() {
            let len = u32::from_le_bytes([rest[1], rest[2], rest[3], 0]) as usize;
            out.push(&rest[8..8 + len]);
            rest = &rest[8 + len..];
        }
        out
    }

    /// A file of `terms` (xorb index, chunk start, chunk end) over `xorbs`,
    /// and its bytes.
    fn file_of(xorbs: &[TestXorb], terms: &[(usize, u32, u32)]) -> (FileRecord, Vec<u8>) {
        let mut bytes = Vec::new();
        let terms = terms
            .iter()
            .map(|&(x, a, b)| {
                let mut unpacked = 0;
                for chunk in &xorbs[x].chunks[a as usize..b as usize] {
                    bytes.extend_from_slice(chunk);
                    unpacked += chunk.len() as u32;
                }
                Term {
                    xorb: xorbs[x].hash,
                    chunk_start: a,
                    chunk_end: b,
                    unpacked_bytes: unpacked,
                }
            })
            .collect();
        let record = FileRecord {
            hash: chunk_hash(&bytes),
            size: bytes.len() as u64,
            terms,
        };
        (record, bytes)
    }

    fn tables(xorbs: &[TestXorb]) -> XorbMap<ChunkLayout> {
        xorbs.iter().map(|x| (x.hash, x.table.clone())).collect()
    }

    /// The [`TermBytes`] the index records for `file`.
    fn term_bytes(file: &FileRecord, tables: &XorbMap<ChunkLayout>) -> TermBytes {
        file.terms
            .iter()
            .map(|t| {
                let offsets = &tables[&t.xorb].offsets;
                (
                    offsets[t.chunk_start as usize],
                    offsets[t.chunk_end as usize],
                )
            })
            .collect()
    }

    /// Checks `plan` against the file it answers, as the client reads it,
    /// and returns the bytes it reconstructs from the range's start on.
    fn check(plan: &Plan, xorbs: &[TestXorb], needed: usize) -> Vec<u8> {
        let by_hash: HashMap<MerkleHash, &TestXorb> = xorbs.iter().map(|x| (x.hash, x)).collect();
        for fetch in &plan.fetches {
            let xorb = by_hash[&fetch.xorb];
            let (a, b) = fetch.chunks;
            assert!(a < b);
            assert_eq!(
                fetch.bytes,
                (
                    u64::from(xorb.table.offsets[a as usize]),
                    u64::from(xorb.table.offsets[b as usize]) - 1
                )
            );
        }
        // Consecutive descriptors of one xorb over adjacent chunks would
        // have been merged.
        for pair in plan.fetches.windows(2) {
            assert!(!(pair[0].xorb == pair[1].xorb && pair[0].chunks.1 == pair[1].chunks.0));
        }
        let mut out = Vec::new();
        for term in &plan.terms {
            let fetch = &plan.fetches[term.fetch];
            assert!(
                fetch.xorb == term.xorb
                    && fetch.chunks.0 <= term.chunks.0
                    && term.chunks.1 <= fetch.chunks.1,
                "every term lies inside the descriptor it names"
            );
            let xorb = by_hash[&term.xorb];
            let (s, e) = fetch.bytes;
            let payloads = decode(&xorb.stored[s as usize..=e as usize]);
            assert_eq!(payloads.len() as u32, fetch.chunks.1 - fetch.chunks.0);
            let lo = (term.chunks.0 - fetch.chunks.0) as usize;
            let hi = (term.chunks.1 - fetch.chunks.0) as usize;
            let data: Vec<u8> = payloads[lo..hi].concat();
            let lengths: u64 = xorb.table.lengths[term.chunks.0 as usize..term.chunks.1 as usize]
                .iter()
                .map(|&len| u64::from(len))
                .sum();
            assert_eq!(u64::from(term.unpacked_length), lengths);
            assert_eq!(data.len() as u64, lengths);
            out.extend(data);
        }
        // Tight: the offset stays inside the first chunk, and the last
        // chunk holds a byte of the range.
        if let (Some(first), Some(last)) = (plan.terms.first(), plan.terms.last()) {
            let first_len = u64::from(by_hash[&first.xorb].table.lengths[first.chunks.0 as usize]);
            assert!(plan.offset_into_first_range < first_len);
            let last_len = by_hash[&last.xorb].table.lengths[last.chunks.1 as usize - 1];
            let offset = plan.offset_into_first_range as usize;
            assert!(out.len() - offset >= needed);
            assert!(out.len() - (last_len as usize) < offset + needed);
        }
        out.drain(..plan.offset_into_first_range as usize);
        out.truncate(needed);
        out
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn plan_property_test(
            lengths in prop::collection::vec(prop::collection::vec(1u32..=40, 1..=20), 1..=5),
            raw_terms in prop::collection::vec((any::<u32>(), any::<u32>(), any::<u32>(), any::<bool>()), 1..=50),
            whole in any::<bool>(),
            start_seed in any::<u64>(),
            len_seed in any::<u64>(),
        ) {
            let xorbs: Vec<TestXorb> =
                lengths.iter().enumerate().map(|(i, l)| test_xorb(i, l)).collect();
            // Biased toward terms continuing the previous one, so merging is
            // exercised.
            let mut terms: Vec<(usize, u32, u32)> = Vec::new();
            for &(x, a, b, follow) in &raw_terms {
                let previous = terms.last().copied();
                let term = match previous {
                    Some((px, _, pb)) if follow && (pb as usize) < xorbs[px].chunks.len() => {
                        let n = xorbs[px].chunks.len() as u32;
                        (px, pb, pb + 1 + b % (n - pb))
                    }
                    _ => {
                        let x = x as usize % xorbs.len();
                        let n = xorbs[x].chunks.len() as u32;
                        let a = a % n;
                        (x, a, a + 1 + b % (n - a))
                    }
                };
                terms.push(term);
            }
            let (file, bytes) = file_of(&xorbs, &terms);
            let size = file.size;
            let range = (!whole).then(|| {
                let start = start_seed % (size + 5);
                (start, start.saturating_add(len_seed % (size + 50)))
            });

            match range {
                Some((start, _)) if start >= size => {
                    prop_assert_eq!(
                        plan(&file, None, &tables(&xorbs), range),
                        Err(PlanError::Unsatisfiable { size })
                    );
                }
                _ => {
                    let (start, end) = range.map_or((0, size - 1), |(s, e)| (s, e.min(size - 1)));
                    let plan = plan(&file, None, &tables(&xorbs), range).unwrap();
                    let needed = (end - start + 1) as usize;
                    let got = check(&plan, &xorbs, needed);
                    prop_assert_eq!(&got[..], &bytes[start as usize..=end as usize]);

                    // From the terms' recorded bytes and only the trimmed
                    // terms' layouts, the plan is the same.
                    let all = tables(&xorbs);
                    let recorded = term_bytes(&file, &all);
                    let window = window(&file, range).unwrap().unwrap();
                    let trimmed: XorbMap<ChunkLayout> = window
                        .trimmed(&file)
                        .iter()
                        .map(|&t| file.terms[t].xorb)
                        .map(|x| (x, all[&x].clone()))
                        .collect();
                    prop_assert!(window.trimmed(&file).len() <= 2);
                    prop_assert_eq!(
                        super::plan(&file, Some(&recorded), &trimmed, range).unwrap(),
                        plan
                    );
                }
            }
        }
    }

    #[test]
    fn adjacent_terms_of_one_xorb_share_a_descriptor() {
        let xorbs = [test_xorb(0, &[3, 4, 5, 6]), test_xorb(1, &[2])];
        let (file, _) = file_of(
            &xorbs,
            &[(0, 0, 2), (0, 2, 3), (1, 0, 1), (0, 3, 4), (0, 0, 1)],
        );
        let plan = plan(&file, None, &tables(&xorbs), None).unwrap();
        let chunks: Vec<(usize, (u32, u32))> = plan
            .fetches
            .iter()
            .map(|f| (usize::from(f.xorb != xorbs[0].hash), f.chunks))
            .collect();
        assert_eq!(chunks, [(0, (0, 3)), (1, (0, 1)), (0, (3, 4)), (0, (0, 1))]);
    }

    #[test]
    fn the_empty_file_has_an_empty_plan_and_no_range() {
        let file = FileRecord {
            hash: MerkleHash::default(),
            size: 0,
            terms: vec![],
        };
        assert_eq!(
            plan(&file, None, &XorbMap::default(), None),
            Ok(Plan::default())
        );
        for range in [(0, 0), (0, u64::MAX), (1, 5)] {
            assert_eq!(
                plan(&file, None, &XorbMap::default(), Some(range)),
                Err(PlanError::Unsatisfiable { size: 0 })
            );
        }
    }

    #[test]
    fn a_record_that_disagrees_with_its_tables_is_inconsistent() {
        let xorbs = [test_xorb(0, &[3, 4])];
        let (file, _) = file_of(&xorbs, &[(0, 0, 2)]);
        let inconsistent = |file: &FileRecord, tables: &XorbMap<ChunkLayout>| {
            matches!(
                plan(file, None, tables, None),
                Err(PlanError::Inconsistent(_))
            )
        };

        assert!(inconsistent(&file, &XorbMap::default()), "a missing table");
        let mut wrong_size = file.clone();
        wrong_size.size += 1;
        assert!(
            inconsistent(&wrong_size, &tables(&xorbs)),
            "terms short of the size"
        );
        let mut past_the_table = file.clone();
        past_the_table.terms[0].chunk_end = 3;
        assert!(
            inconsistent(&past_the_table, &tables(&xorbs)),
            "chunks past the table"
        );
        let mut longer = file.clone();
        longer.terms[0].unpacked_bytes += 1;
        longer.size += 1;
        assert!(
            inconsistent(&longer, &tables(&xorbs)),
            "a term longer than its chunks"
        );
    }
}
