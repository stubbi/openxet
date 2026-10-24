//! The JSON bodies of reconstruction answers (S10 §1): v2's
//! `{offset_into_first_range, terms, xorbs}`, v1's
//! `{offset_into_first_range, terms, fetch_info}` and the batch's
//! `{files, fetch_info}`.
//!
//! A file of 100k terms answers about 42 MB carrying 100k signed URLs, and
//! design §5.8 gives the whole request 50 ms at p99. So the body is written
//! by hand into one buffer sized up front, and each URL is signed straight
//! into it. The bytes are what `serde_json` writes for the same shapes,
//! compact and with fields in this order; the tests pin that.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

use axum::body::Bytes;
use foldhash::fast::RandomState;
use openxet_auth::{TransferUrls, UrlSigner, XorbUrls};
use openxet_protocol::{MerkleHash, hash_to_hex};
use url::Url;

use crate::plan::{Fetch, Plan, PlannedTerm, XorbMap};
use crate::{AppState, CasError};

/// Which answer shape a single-file reconstruction takes (S10 §1).
#[derive(Debug, Clone, Copy)]
pub(crate) enum Version {
    /// `xorbs`: a list of `{url, ranges: [descriptor]}` per xorb.
    V2,
    /// `fetch_info`: a list of `{range, url, url_range}` per xorb.
    V1,
}

/// What signing a transfer URL needs, owned for a blocking thread.
pub(crate) struct Signer {
    urls: Arc<UrlSigner>,
    base: Url,
    now: u64,
}

impl Signer {
    pub(crate) fn new(state: &AppState) -> Self {
        Self {
            urls: Arc::clone(&state.urls),
            base: state.cfg.public_url.clone(),
            now: state.clock.now(),
        }
    }
}

/// The single-file answer to `plan`, in `version`'s shape, written by
/// `workers` threads.
pub(crate) fn single(
    plan: &Plan,
    version: Version,
    signer: &Signer,
    workers: usize,
) -> Result<Vec<Bytes>, CasError> {
    let mut catalog = Catalog::default();
    let names = catalog.add(plan);
    let entries = Entries::new(&catalog, version, signer);
    let groups = catalog.split(workers);
    let terms = split(plan.terms.len(), workers);
    let written = in_parallel(workers, |w| {
        let mut list = Json::with_capacity(terms[w].len() * TERM_BYTES);
        list.terms(
            &plan.terms[terms[w].clone()],
            terms[w].start == 0,
            &names,
            &catalog,
        );
        let mut map = Json::with_capacity(entries.capacity(&groups[w]));
        map.entries(&entries, groups[w].clone())?;
        Ok::<_, CasError>((list.0, map.0))
    });
    let (lists, maps): (Vec<String>, Vec<String>) = written
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .unzip();

    let mut head = Json::with_capacity(64);
    head.raw(r#"{"offset_into_first_range":"#);
    head.num(plan.offset_into_first_range);
    head.raw(r#","terms":["#);
    let middle = match version {
        Version::V2 => r#"],"xorbs":{"#,
        Version::V1 => r#"],"fetch_info":{"#,
    };
    Ok(frames(
        [head.0]
            .into_iter()
            .chain(lists)
            .chain([middle.to_owned()])
            .chain(maps)
            .chain(["}}".to_owned()]),
    ))
}

/// The batch answer for `plans`, each a file and its whole-file plan, in
/// the v1 shapes. Its `fetch_info` is written by `workers` threads.
pub(crate) fn batch(
    plans: &[(MerkleHash, Plan)],
    signer: &Signer,
    workers: usize,
) -> Result<Vec<Bytes>, CasError> {
    let mut catalog = Catalog::default();
    let names: Vec<Vec<usize>> = plans.iter().map(|(_, plan)| catalog.add(plan)).collect();
    let terms: usize = plans.iter().map(|(_, plan)| plan.terms.len()).sum();
    let mut files = Json::with_capacity(terms * TERM_BYTES + plans.len() * NAME_BYTES + 64);
    files.raw(r#"{"files":{"#);
    for (i, ((file, plan), names)) in plans.iter().zip(&names).enumerate() {
        if i > 0 {
            files.raw(",");
        }
        files.string(&hash_to_hex(file));
        files.raw(":[");
        files.terms(&plan.terms, true, names, &catalog);
        files.raw("]");
    }
    files.raw(r#"},"fetch_info":{"#);
    let entries = Entries::new(&catalog, Version::V1, signer);
    let groups = catalog.split(workers);
    let maps = in_parallel(workers, |w| {
        let mut map = Json::with_capacity(entries.capacity(&groups[w]));
        map.entries(&entries, groups[w].clone())?;
        Ok::<_, CasError>(map.0)
    });
    let maps = maps.into_iter().collect::<Result<Vec<_>, _>>()?;
    Ok(frames(
        [files.0].into_iter().chain(maps).chain(["}}".to_owned()]),
    ))
}

/// How many threads should write an answer of `terms` terms: one per
/// [`TERMS_PER_WORKER`], at least one. Each thread past the first runs
/// under a `validate` permit of its own (rulings M1-R8, M1-R25).
pub(crate) fn workers_for(terms: usize) -> usize {
    terms.div_ceil(TERMS_PER_WORKER).max(1)
}

/// Terms one writer thread is worth: below this, starting a thread costs
/// more than it saves.
const TERMS_PER_WORKER: usize = 16 * 1024;

/// The non-empty `parts` as body frames.
fn frames(parts: impl IntoIterator<Item = String>) -> Vec<Bytes> {
    parts
        .into_iter()
        .filter(|part| !part.is_empty())
        .map(Bytes::from)
        .collect()
}

/// `work(w)` for every `w` below `workers`, each on its own thread but the
/// first, which runs on this one; the results in order of `w`. A panic in
/// any is resumed here.
fn in_parallel<T: Send>(workers: usize, work: impl Fn(usize) -> T + Sync) -> Vec<T> {
    if workers <= 1 {
        return vec![work(0)];
    }
    std::thread::scope(|scope| {
        let work = &work;
        let others: Vec<_> = (1..workers).map(|w| scope.spawn(move || work(w))).collect();
        let first = work(0);
        std::iter::once(first)
            .chain(others.into_iter().map(|t| {
                t.join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            }))
            .collect()
    })
}

/// `0..len` in `parts` contiguous ranges of near-equal length.
fn split(len: usize, parts: usize) -> Vec<Range<usize>> {
    (0..parts)
        .map(|p| len * p / parts..len * (p + 1) / parts)
        .collect()
}

/// The fetch descriptors of one answer, grouped by xorb. A descriptor two
/// terms or two files share is listed, and signed, once.
#[derive(Default)]
struct Catalog {
    /// The distinct xorbs, in order of first use, and their hex names.
    xorbs: Vec<(MerkleHash, String)>,
    /// Where each xorb is in `xorbs`.
    index: XorbMap<usize>,
    /// Per entry of `xorbs`, its descriptors' indices in `fetches`.
    groups: Vec<Vec<usize>>,
    fetches: Vec<Fetch>,
    /// The listed descriptors, by xorb index and chunk range. A chunk range
    /// is the uploader's choice, but as with [`XorbMap`] each request's set
    /// has its own random seed, so no list of them collides in every set.
    seen: HashSet<(usize, (u32, u32)), RandomState>,
}

impl Catalog {
    /// Adds `plan`'s descriptors, and returns for each the index of its
    /// xorb in `xorbs`, which names the terms it holds.
    fn add(&mut self, plan: &Plan) -> Vec<usize> {
        self.seen.reserve(plan.fetches.len());
        plan.fetches
            .iter()
            .map(|fetch| {
                let xorb = self.xorb(&fetch.xorb);
                if self.seen.insert((xorb, fetch.chunks)) {
                    self.groups[xorb].push(self.fetches.len());
                    self.fetches.push(*fetch);
                }
                xorb
            })
            .collect()
    }

    fn xorb(&mut self, xorb: &MerkleHash) -> usize {
        *self.index.entry(*xorb).or_insert_with(|| {
            self.xorbs.push((*xorb, hash_to_hex(xorb)));
            self.groups.push(Vec::new());
            self.xorbs.len() - 1
        })
    }

    /// The xorbs in `parts` contiguous ranges holding near-equal numbers of
    /// descriptors, for writing their entries in parallel.
    fn split(&self, parts: usize) -> Vec<Range<usize>> {
        let total = self.fetches.len();
        let mut ranges = Vec::with_capacity(parts);
        let (mut start, mut seen) = (0, 0);
        for (i, group) in self.groups.iter().enumerate() {
            seen += group.len();
            if ranges.len() + 1 < parts && seen * parts >= total * (ranges.len() + 1) {
                ranges.push(start..i + 1);
                start = i + 1;
            }
        }
        ranges.push(start..self.groups.len());
        ranges.resize(parts, self.groups.len()..self.groups.len());
        ranges
    }
}

/// What writing the catalog's entries needs, shared by the writer threads.
struct Entries<'a> {
    catalog: &'a Catalog,
    version: Version,
    urls: TransferUrls<'a>,
    /// Whether the URLs' base needs a JSON escape ([`json_safe`]).
    escape: bool,
    base_len: usize,
}

impl<'a> Entries<'a> {
    fn new(catalog: &'a Catalog, version: Version, signer: &'a Signer) -> Self {
        Self {
            catalog,
            version,
            urls: signer.urls.transfer_urls(&signer.base, signer.now),
            escape: !json_safe(signer.base.as_str()),
            base_len: signer.base.as_str().len(),
        }
    }

    /// An upper bound on the bytes of the entries of `xorbs`.
    fn capacity(&self, xorbs: &Range<usize>) -> usize {
        let fetches: usize = self.catalog.groups[xorbs.clone()]
            .iter()
            .map(Vec::len)
            .sum();
        fetches * (FETCH_BYTES + self.base_len) + xorbs.len() * NAME_BYTES
    }
}

/// An answer being written. Every string written is ASCII that needs no
/// JSON escape (hex, digits, base64url and fixed keys), except a URL whose
/// base needs one ([`json_safe`]).
struct Json(String);

/// Generous upper bounds on each part's bytes, so the buffer is allocated
/// once: a term is at most 147 bytes, a descriptor 270 plus its URL's base,
/// a xorb's key or a file's 68.
const TERM_BYTES: usize = 160;
const FETCH_BYTES: usize = 300;
const NAME_BYTES: usize = 70;

impl Json {
    fn with_capacity(bytes: usize) -> Self {
        Self(String::with_capacity(bytes))
    }

    fn raw(&mut self, s: &str) {
        self.0.push_str(s);
    }

    fn num<N: itoa::Integer>(&mut self, n: N) {
        self.0.push_str(itoa::Buffer::new().format(n));
    }

    /// A string that needs no escape.
    fn string(&mut self, s: &str) {
        self.0.push('"');
        self.0.push_str(s);
        self.0.push('"');
    }

    /// `{"start":…,"end":…}`.
    fn span<N: itoa::Integer>(&mut self, (start, end): (N, N)) {
        self.raw(r#"{"start":"#);
        self.num(start);
        self.raw(r#","end":"#);
        self.num(end);
        self.raw("}");
    }

    /// `terms` as items of a JSON list, each `{hash, unpacked_length,
    /// range}`, the first after a comma unless `first`. `names` holds the
    /// catalog's xorb index of each of their plan's descriptors.
    fn terms(&mut self, terms: &[PlannedTerm], first: bool, names: &[usize], catalog: &Catalog) {
        for (i, term) in terms.iter().enumerate() {
            if i > 0 || !first {
                self.raw(",");
            }
            self.raw(r#"{"hash":"#);
            self.string(&catalog.xorbs[names[term.fetch]].1);
            self.raw(r#","unpacked_length":"#);
            self.num(term.unpacked_length);
            self.raw(r#","range":"#);
            self.span(term.chunks);
            self.raw("}");
        }
    }

    /// The entries of the catalog's xorbs `xorbs` as members of a JSON
    /// object, v2's `xorbs` or v1's `fetch_info`: `"{xorb}":[entry…]`, the
    /// first after a comma unless `xorbs` starts at the first xorb. Each
    /// descriptor gets its own URL, signed for exactly its byte range (ADR
    /// 0002), one xorb at a time so each xorb's part of the MAC is computed
    /// once.
    fn entries(&mut self, entries: &Entries<'_>, xorbs: Range<usize>) -> Result<(), CasError> {
        let catalog = entries.catalog;
        let mut escaped = entries.escape.then(String::new);
        for i in xorbs {
            if i > 0 {
                self.raw(",");
            }
            let (xorb, name) = &catalog.xorbs[i];
            self.string(name);
            self.raw(":[");
            let xorb = entries.urls.xorb(xorb);
            for (j, &f) in catalog.groups[i].iter().enumerate() {
                if j > 0 {
                    self.raw(",");
                }
                let fetch = &catalog.fetches[f];
                match entries.version {
                    Version::V2 => {
                        self.raw(r#"{"url":"#);
                        self.url(&xorb, fetch, escaped.as_mut())?;
                        self.raw(r#","ranges":[{"chunks":"#);
                        self.span(fetch.chunks);
                        self.raw(r#","bytes":"#);
                        self.span(fetch.bytes);
                        self.raw("}]}");
                    }
                    Version::V1 => {
                        self.raw(r#"{"range":"#);
                        self.span(fetch.chunks);
                        self.raw(r#","url":"#);
                        self.url(&xorb, fetch, escaped.as_mut())?;
                        self.raw(r#","url_range":"#);
                        self.span(fetch.bytes);
                        self.raw("}");
                    }
                }
            }
            self.raw("]");
        }
        Ok(())
    }

    /// `fetch`'s signed URL as a JSON string. With `escaped`, a scratch
    /// buffer for a base that needs escaping, it is signed there first and
    /// escaped as `serde_json` escapes it.
    fn url(
        &mut self,
        xorb: &XorbUrls<'_>,
        fetch: &Fetch,
        escaped: Option<&mut String>,
    ) -> Result<(), CasError> {
        let (start, end) = fetch.bytes;
        match escaped {
            None => {
                self.0.push('"');
                xorb.push_signed(start, end, &mut self.0)?;
                self.0.push('"');
            }
            Some(scratch) => {
                scratch.clear();
                xorb.push_signed(start, end, scratch)?;
                let quoted = serde_json::to_string(scratch).map_err(CasError::internal)?;
                self.raw(&quoted);
            }
        }
        Ok(())
    }
}

/// Whether `s` needs no escape inside a JSON string (RFC 8259 §7). A
/// serialized URL can still hold a `"`: the URL standard allows it in a
/// host.
fn json_safe(s: &str) -> bool {
    !s.bytes().any(|b| b == b'"' || b == b'\\' || b < 0x20)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use openxet_protocol::chunk_hash;
    use proptest::prelude::*;
    use serde::Serialize;
    use serde::ser::{SerializeStruct as _, Serializer};

    use super::*;

    /// The answers as `serde_json` serialized them before the hand-written
    /// writer: the reference the writer must match byte for byte.
    mod reference {
        use super::*;

        #[derive(Serialize)]
        struct Span<T> {
            start: T,
            end: T,
        }

        #[derive(Serialize)]
        struct TermJson {
            hash: String,
            unpacked_length: u32,
            range: Span<u32>,
        }

        #[derive(Serialize)]
        struct Descriptor {
            chunks: Span<u32>,
            bytes: Span<u64>,
        }

        #[derive(Serialize)]
        struct EntryV2<'a> {
            url: &'a str,
            ranges: [Descriptor; 1],
        }

        #[derive(Serialize)]
        struct EntryV1<'a> {
            range: Span<u32>,
            url: &'a str,
            url_range: Span<u64>,
        }

        /// The old catalog: descriptors deduplicated by xorb and chunks,
        /// grouped by xorb in order of first use, each URL from
        /// `sign_xorb_range`.
        struct Old {
            groups: Vec<(MerkleHash, Vec<(Fetch, String)>)>,
        }

        impl Old {
            fn new(plans: &[&Plan], signer: &Signer) -> Self {
                let mut seen = HashSet::new();
                let mut groups: Vec<(MerkleHash, Vec<(Fetch, String)>)> = Vec::new();
                for fetch in plans.iter().flat_map(|p| &p.fetches) {
                    if !seen.insert((fetch.xorb, fetch.chunks)) {
                        continue;
                    }
                    let url = signer
                        .urls
                        .sign_xorb_range(
                            &signer.base,
                            &fetch.xorb,
                            fetch.bytes.0,
                            fetch.bytes.1,
                            signer.now,
                        )
                        .unwrap()
                        .to_string();
                    match groups.iter_mut().find(|(x, _)| *x == fetch.xorb) {
                        Some((_, group)) => group.push((*fetch, url)),
                        None => groups.push((fetch.xorb, vec![(*fetch, url)])),
                    }
                }
                Self { groups }
            }
        }

        struct Terms<'a>(&'a [PlannedTerm]);

        impl Serialize for Terms<'_> {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_seq(self.0.iter().map(|t| TermJson {
                    hash: hash_to_hex(&t.xorb),
                    unpacked_length: t.unpacked_length,
                    range: Span {
                        start: t.chunks.0,
                        end: t.chunks.1,
                    },
                }))
            }
        }

        struct Entries<'a>(&'a Old, Version);

        impl Serialize for Entries<'_> {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                let version = self.1;
                s.collect_map(
                    self.0
                        .groups
                        .iter()
                        .map(|(xorb, group)| (hash_to_hex(xorb), Group(group, version))),
                )
            }
        }

        struct Group<'a>(&'a [(Fetch, String)], Version);

        impl Serialize for Group<'_> {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                let entries = self.0.iter().map(|(f, url)| {
                    (
                        url.as_str(),
                        Span {
                            start: f.chunks.0,
                            end: f.chunks.1,
                        },
                        Span {
                            start: f.bytes.0,
                            end: f.bytes.1,
                        },
                    )
                });
                match self.1 {
                    Version::V2 => s.collect_seq(entries.map(|(url, chunks, bytes)| EntryV2 {
                        url,
                        ranges: [Descriptor { chunks, bytes }],
                    })),
                    Version::V1 => s.collect_seq(entries.map(|(url, range, url_range)| EntryV1 {
                        range,
                        url,
                        url_range,
                    })),
                }
            }
        }

        pub(super) fn single(plan: &Plan, version: Version, signer: &Signer) -> Vec<u8> {
            struct Single<'a>(&'a Plan, &'a Old, Version);
            impl Serialize for Single<'_> {
                fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                    let mut out = s.serialize_struct("Reconstruction", 3)?;
                    out.serialize_field(
                        "offset_into_first_range",
                        &self.0.offset_into_first_range,
                    )?;
                    out.serialize_field("terms", &Terms(&self.0.terms))?;
                    let entries = Entries(self.1, self.2);
                    match self.2 {
                        Version::V2 => out.serialize_field("xorbs", &entries)?,
                        Version::V1 => out.serialize_field("fetch_info", &entries)?,
                    }
                    out.end()
                }
            }
            let old = Old::new(&[plan], signer);
            serde_json::to_vec(&Single(plan, &old, version)).unwrap()
        }

        pub(super) fn batch(plans: &[(MerkleHash, Plan)], signer: &Signer) -> Vec<u8> {
            struct Files<'a>(&'a [(MerkleHash, Plan)]);
            impl Serialize for Files<'_> {
                fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                    s.collect_map(
                        self.0
                            .iter()
                            .map(|(file, plan)| (hash_to_hex(file), Terms(&plan.terms))),
                    )
                }
            }
            struct Batch<'a>(&'a [(MerkleHash, Plan)], &'a Old);
            impl Serialize for Batch<'_> {
                fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                    let mut out = s.serialize_struct("BatchReconstruction", 2)?;
                    out.serialize_field("files", &Files(self.0))?;
                    out.serialize_field("fetch_info", &Entries(self.1, Version::V1))?;
                    out.end()
                }
            }
            let refs: Vec<&Plan> = plans.iter().map(|(_, p)| p).collect();
            let old = Old::new(&refs, signer);
            serde_json::to_vec(&Batch(plans, &old)).unwrap()
        }
    }

    fn signer(base: &str, now: u64) -> Signer {
        Signer {
            urls: Arc::new(UrlSigner::new([5; 32], UrlSigner::DEFAULT_LIFETIME).unwrap()),
            base: Url::parse(base).unwrap(),
            now,
        }
    }

    /// A plan of `terms`, each `(xorb, chunk start, chunk count, merge)`:
    /// a term continuing the previous one's xorb and chunks shares its
    /// descriptor when `merge` is set, as the planner merges.
    fn plan_of(offset: u64, terms: &[(u8, u32, u32, bool)]) -> Plan {
        let mut plan = Plan {
            offset_into_first_range: offset,
            ..Plan::default()
        };
        for &(x, start, count, merge) in terms {
            let xorb = chunk_hash(&[x]);
            let chunks = (start, start + count);
            let bytes = (u64::from(start) * 1000, u64::from(start + count) * 1000 - 1);
            match plan.fetches.last_mut() {
                Some(last) if merge && last.xorb == xorb && last.chunks.1 == start => {
                    last.chunks.1 = chunks.1;
                    last.bytes.1 = bytes.1;
                }
                _ => plan.fetches.push(Fetch {
                    xorb,
                    chunks,
                    bytes,
                }),
            }
            plan.terms.push(PlannedTerm {
                xorb,
                chunks,
                unpacked_length: count * 999 + u32::from(x),
                fetch: plan.fetches.len() - 1,
            });
        }
        plan
    }

    fn terms_strategy() -> impl Strategy<Value = Vec<(u8, u32, u32, bool)>> {
        prop::collection::vec((0u8..6, 0u32..40, 1u32..5, any::<bool>()), 0..40)
    }

    const BASES: &[&str] = &[
        "https://cas.example.com",
        "http://127.0.0.1:8080/prefix/",
        // A `"` survives in a host, so this base's URLs need escaping.
        "http://a\"b.example/",
    ];

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn single_answers_equal_serde_json_byte_for_byte(
            terms in terms_strategy(),
            offset in any::<u64>(),
            base in 0..BASES.len(),
            now in prop_oneof![Just(u64::MAX), 0u64..(1 << 40)],
            workers in 1usize..6,
        ) {
            let signer = signer(BASES[base], now);
            let plan = plan_of(offset, &terms);
            for version in [Version::V2, Version::V1] {
                let ours = single(&plan, version, &signer, workers).unwrap().concat();
                let theirs = reference::single(&plan, version, &signer);
                prop_assert_eq!(String::from_utf8(ours).unwrap(), String::from_utf8(theirs).unwrap());
            }
        }

        #[test]
        fn batch_answers_equal_serde_json_byte_for_byte(
            files in prop::collection::vec(terms_strategy(), 0..5),
            base in 0..BASES.len(),
            workers in 1usize..6,
        ) {
            let signer = signer(BASES[base], 1_800_000_000);
            let plans: Vec<(MerkleHash, Plan)> = files
                .iter()
                .zip(100u8..)
                .map(|(terms, i)| (chunk_hash(&[i]), plan_of(0, terms)))
                .collect();
            let ours = batch(&plans, &signer, workers).unwrap().concat();
            let theirs = reference::batch(&plans, &signer);
            prop_assert_eq!(String::from_utf8(ours).unwrap(), String::from_utf8(theirs).unwrap());
        }
    }

    #[test]
    fn a_base_that_needs_escaping_is_escaped() {
        assert!(json_safe("https://cas.example.com/p%22q"));
        assert!(!json_safe("http://a\"b.example/"));
        let plan = plan_of(0, &[(1, 0, 2, false)]);
        let body = single(&plan, Version::V2, &signer(BASES[2], 0), 2)
            .unwrap()
            .concat();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let url = json["xorbs"].as_object().unwrap().values().next().unwrap()[0]["url"]
            .as_str()
            .unwrap();
        assert!(
            url.starts_with("http://a\"b.example/v1/transfer/xorbs/"),
            "{url}"
        );
    }
}
