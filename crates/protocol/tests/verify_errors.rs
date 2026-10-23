//! What a [`VerifyError`] tells a client (ruling M1-R23).
//!
//! Verification is the proof that an uploader holds the chunks it
//! references. Its client-facing text (`Display`) names the rule, the file
//! and xorb hashes and the chunk range the client sent; any value the server
//! derived from its stored chunks is kept for logs ([`VerifyError::detail`]).

#![allow(clippy::unwrap_used)]

use openxet_protocol::{MerkleHash, VerifyError, chunk_hash, hash_to_hex};

/// A hash no client sent: it stands for one the server computed.
fn derived(name: &str) -> MerkleHash {
    chunk_hash(format!("server-derived {name}").as_bytes())
}

fn claimed(name: &str) -> MerkleHash {
    chunk_hash(format!("client-claimed {name}").as_bytes())
}

/// Every variant, with its server-derived values.
fn cases() -> Vec<(VerifyError, Vec<String>)> {
    let (file, xorb) = (claimed("file"), claimed("xorb"));
    vec![
        (
            VerifyError::OutOfBounds {
                xorb,
                start: 3,
                end: 9,
                xorb_chunks: 7_654_321,
            },
            vec!["7654321".to_owned()],
        ),
        (
            VerifyError::MissingVerification {
                xorb,
                start: 3,
                end: 9,
            },
            vec![],
        ),
        (
            VerifyError::RangeHashMismatch {
                xorb,
                start: 3,
                end: 9,
                expected: derived("range"),
                actual: claimed("range"),
            },
            vec![hash_to_hex(&derived("range"))],
        ),
        (
            VerifyError::LengthMismatch {
                xorb,
                start: 3,
                end: 9,
                expected: 8_765_432,
                actual: 1_234_567,
            },
            vec!["8765432".to_owned()],
        ),
        (VerifyError::UnknownXorb { file, xorb }, vec![]),
        (
            VerifyError::FileHashMismatch {
                expected: derived("file"),
                actual: file,
            },
            vec![hash_to_hex(&derived("file"))],
        ),
        (
            VerifyError::EmptyFileSha256 {
                file,
                actual: "ab".repeat(32),
            },
            vec![],
        ),
        (VerifyError::ConflictingFileSha256 { file }, vec![]),
        (
            VerifyError::TooMuchWork {
                referenced_chunks: 99,
                limit: 10,
            },
            vec![],
        ),
        (
            VerifyError::TooManyXorbs {
                referenced_xorbs: 99,
                limit: 10,
            },
            vec![],
        ),
    ]
}

#[test]
fn verification_errors_reveal_no_server_derived_values() {
    for (error, derived) in cases() {
        let client = error.to_string();
        for value in &derived {
            assert!(
                !client.contains(value.as_str()),
                "{error:?} leaks: {client}"
            );
            assert!(error.detail().contains(value.as_str()), "{error:?}");
        }
        assert!(!client.is_empty());
    }
}

#[test]
fn verification_errors_name_what_the_client_sent() {
    let (file, xorb) = (hash_to_hex(&claimed("file")), hash_to_hex(&claimed("xorb")));
    for (error, _) in cases() {
        let client = error.to_string();
        match &error {
            VerifyError::OutOfBounds { .. }
            | VerifyError::MissingVerification { .. }
            | VerifyError::RangeHashMismatch { .. }
            | VerifyError::LengthMismatch { .. } => {
                assert!(client.contains(&format!("{xorb}[3..9]")), "{client}");
                assert!(client.contains("rebuild the shard"), "{client}");
            }
            VerifyError::UnknownXorb { .. } => {
                assert!(client.contains(&xorb) && client.contains(&file), "{client}");
            }
            VerifyError::FileHashMismatch { .. } => {
                assert!(client.contains(&file), "{client}");
                assert!(client.contains("rebuild the shard"), "{client}");
            }
            VerifyError::EmptyFileSha256 { .. } | VerifyError::ConflictingFileSha256 { .. } => {
                assert!(client.contains(&file), "{client}");
            }
            VerifyError::TooMuchWork { .. } | VerifyError::TooManyXorbs { .. } => {
                assert!(client.contains("split the upload"), "{client}");
            }
        }
    }
}
