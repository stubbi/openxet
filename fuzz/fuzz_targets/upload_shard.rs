//! Upload shard parsing: any bytes, never a panic.

#![no_main]

use libfuzzer_sys::fuzz_target;
use openxet_protocol::read_upload_shard;

fuzz_target!(|data: &[u8]| {
    let _ = read_upload_shard(data);
});
