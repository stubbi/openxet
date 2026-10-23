//! Shard verification and re-serialization against the `hf_xet` capture's
//! stored xorb: any parsed shard verifies or is rejected without a panic, and
//! a verified shard's stored form parses again.

#![no_main]

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use libfuzzer_sys::fuzz_target;
use openxet_protocol::{
    MerkleHash, ProtocolError, VerifyLimits, read_upload_shard, read_xorb_chunks, stored_shard,
    verify_shard, xorb_hash,
};

type Lookup = HashMap<MerkleHash, Arc<[(MerkleHash, u64)]>>;

const CAPTURE_XORB: &[u8] = include_bytes!(
    "../../testdata/hf-xet-1.7.0-upload/43dcdd6c959fc72d974ab1a08aeaa67de0d43fec6b5694598419af0be2f7040c.xorb"
);

fn lookup() -> &'static Lookup {
    static LOOKUP: OnceLock<Lookup> = OnceLock::new();
    LOOKUP.get_or_init(|| {
        let chunks = read_xorb_chunks(CAPTURE_XORB).expect("capture xorb parses");
        let hash = xorb_hash(&chunks);
        HashMap::from([(hash, Arc::from(chunks.hashes))])
    })
}

fuzz_target!(|data: &[u8]| {
    let Ok(shard) = read_upload_shard(data) else {
        return;
    };
    let lookup = lookup();
    if verify_shard(&shard, lookup, VerifyLimits::default()).is_err() {
        return;
    }
    // Two entries for one file hash with different SHA-256 values pass
    // verification and are rejected here; that is the documented contract.
    let stored = match stored_shard(&shard, lookup) {
        Ok(stored) => stored,
        Err(ProtocolError::ShardFileSha256Conflict { .. }) => return,
        Err(err) => panic!("a verified shard has a stored form: {err}"),
    };
    read_upload_shard(stored.as_slice()).expect("a stored shard parses");
});
