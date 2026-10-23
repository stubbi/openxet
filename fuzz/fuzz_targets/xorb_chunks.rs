//! Footer-less xorb chunk reading: any bytes, never a panic.

#![no_main]

use libfuzzer_sys::fuzz_target;
use openxet_protocol::read_xorb_chunks;

fuzz_target!(|data: &[u8]| {
    let _ = read_xorb_chunks(data);
});
