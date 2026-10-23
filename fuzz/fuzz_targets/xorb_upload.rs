//! `POST /v1/xorbs` body validation: any bytes, never a panic.

#![no_main]

use libfuzzer_sys::fuzz_target;
use openxet_protocol::xorb_with_footer;

fuzz_target!(|data: &[u8]| {
    let _ = xorb_with_footer(data);
});
