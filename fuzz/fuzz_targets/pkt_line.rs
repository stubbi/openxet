//! pkt-line framing and the request parsers openxet runs on untrusted
//! bytes before git sees them (ADR 0004 item 2): `read_pkt`, the resumable
//! `find_flush` the interposer uses on a body arriving in pieces,
//! `parse_v2_command` and `parse_v0_wants`. None panics; framing lengths
//! stay inside the input; scanning in two pieces finds what one scan
//! finds; an `ERR` packet of any text reads back as one data line.

#![no_main]

use libfuzzer_sys::fuzz_target;
use openxet_git::{Pkt, err_pkt, find_flush, parse_v0_wants, parse_v2_command, read_pkt};

fuzz_target!(|data: &[u8]| {
    let mut pos = 0;
    while let Ok(Some((pkt, len))) = read_pkt(&data[pos..]) {
        assert!(len >= 4 && pos + len <= data.len());
        if let Pkt::Data(line) = pkt {
            assert_eq!(line.len(), len - 4);
        }
        pos += len;
    }

    let whole = find_flush(data, 0);
    if let Ok(Ok(end)) = &whole {
        assert!(*end <= data.len());
    }
    if let Ok(Err(resume)) = find_flush(&data[..data.len() / 2], 0) {
        assert_eq!(find_flush(data, resume), whole);
    }

    // Errors go back to the client, quoting at most a short piece.
    if let Err(e) = parse_v2_command(data) {
        assert!(e.to_string().len() < 512);
    }
    if let Err(e) = parse_v0_wants(data) {
        assert!(e.to_string().len() < 512);
    }

    let text = String::from_utf8_lossy(data);
    let err = err_pkt(&text);
    match read_pkt(&err) {
        Ok(Some((Pkt::Data(line), len))) => {
            assert_eq!(len, err.len());
            assert!(line.starts_with(b"ERR "));
        }
        other => panic!("an ERR packet reads back as {other:?}"),
    }
});
