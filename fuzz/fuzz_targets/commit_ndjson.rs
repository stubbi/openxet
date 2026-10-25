//! The Hub commit body parser, `openxet_hub::parse_commit_ndjson`, on
//! untrusted bytes (ruling M2-R7). Byte 0 picks the regular-file limit
//! (0 to 255 bytes) and byte 1 the operation limit (0 to 255), so small
//! inputs reach both refusals; the rest is the body. It never panics, and
//! what it accepts has a header, at least one and at most the limit of
//! operations, and no `file` content over the limit.

#![no_main]

use libfuzzer_sys::fuzz_target;
use openxet_hub::{Limits, parse_commit_ndjson};

fuzz_target!(|data: &[u8]| {
    let [file_limit, ops_limit, body @ ..] = data else {
        return;
    };
    let limits = Limits {
        max_file_bytes: u64::from(*file_limit),
        max_ops: usize::from(*ops_limit),
    };
    if let Ok(request) = parse_commit_ndjson(body, &limits) {
        assert!(!request.ops.is_empty() && request.ops.len() <= limits.max_ops);
        for op in &request.ops {
            if let openxet_git::Op::File { content, .. } = op {
                assert!(content.len() <= usize::from(*file_limit));
            }
        }
    }
});
