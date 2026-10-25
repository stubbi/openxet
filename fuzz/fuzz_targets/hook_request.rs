//! The parent's reading of a hook request on untrusted bytes
//! (`openxet_git::hook::parse_hook_request`, ADR 0004 item 7): it never
//! panics; a request without the exact token is refused; an accepted one
//! has at least one command and takes nothing but commands and pack paths
//! from the hook.

#![no_main]

use libfuzzer_sys::fuzz_target;
use openxet_git::hook::parse_hook_request;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fuzz_target!(|data: &[u8]| {
    if let Ok(request) = parse_hook_request(data, TOKEN) {
        assert!(!request.commands.is_empty());
        // The length prefix is hex in either case; the payload is exact.
        let payload = format!("token {TOKEN}");
        let len = usize::from_str_radix(std::str::from_utf8(&data[..4]).unwrap(), 16).unwrap();
        assert_eq!(len, payload.len() + 4);
        assert_eq!(&data[4..len], payload.as_bytes());
    }
});
