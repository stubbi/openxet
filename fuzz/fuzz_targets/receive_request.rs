//! The receive-pack request parser on untrusted bytes (`RequestParser`,
//! crates/git/src/request.rs): it never panics, the pack it measures is
//! never longer than the input, a header count implies a whole header, and
//! the result doesn't depend on how the body was split into chunks.
//! Byte 0 picks the chunk size for the split run.

#![no_main]

use libfuzzer_sys::fuzz_target;
use openxet_git::RequestParser;

fuzz_target!(|data: &[u8]| {
    let Some((&split, body)) = data.split_first() else {
        return;
    };
    let mut whole = RequestParser::default();
    whole.push(body);
    let whole = whole.finish();

    let mut chunked = RequestParser::default();
    for chunk in body.chunks(usize::from(split).max(1)) {
        chunked.push(chunk);
    }
    assert_eq!(chunked.finish(), whole);

    assert!(whole.pack_bytes <= body.len() as u64);
    if whole.header_count.is_some() {
        assert!(whole.pack_bytes >= 12);
    } else {
        assert_eq!(whole.pack_bytes, 0);
    }
});
