//! The reconstruction routes' `Range` parsing (RFC 9110 §14.1.2; S10 §1)
//! on untrusted headers: the first 8 bytes are the file size, and the rest,
//! split at newlines, are `Range` header values. It never panics, and an
//! accepted range starts no later than it ends, and a suffix range starts
//! within the file.

#![no_main]

use http::header::{HeaderMap, HeaderValue, RANGE};
use libfuzzer_sys::fuzz_target;
use openxet_cas::reconstruction_range;

fuzz_target!(|data: &[u8]| {
    let Some((size, values)) = data.split_first_chunk::<8>() else {
        return;
    };
    let size = u64::from_le_bytes(*size);
    let mut headers = HeaderMap::new();
    for value in values.split(|&b| b == b'\n') {
        let Ok(value) = HeaderValue::from_bytes(value) else {
            return;
        };
        headers.append(RANGE, value);
    }
    if let Ok(Some((start, end))) = reconstruction_range(&headers, size) {
        assert!(start <= end, "{headers:?} gave {start}-{end}");
        let suffix = headers
            .get(RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split_once('='))
            .is_some_and(|(_, spec)| spec.trim().starts_with('-'));
        if suffix {
            assert!(start < size, "{headers:?} gave {start}-{end} of {size}");
        }
    }
});
