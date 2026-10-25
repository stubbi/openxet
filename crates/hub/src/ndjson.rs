//! The body of `POST /api/{type}s/{id}/commit/{rev}` (ruling M2-R7):
//! NDJSON, one `{"key", "value"}` object per line, a `header` first, then
//! the operations (`_commit_api.py` lines 932–968, S3 fixtures `09-…-14`
//! and `10-…-02`). `huggingface_hub` writes each line with Python's
//! default separators (`", "` and `": "`), so lines are parsed as JSON,
//! never matched by byte pattern.
//!
//! The body is untrusted: it is bounded before it is parsed, every line is
//! checked, and no message quotes what the client sent.

use axum::body::Bytes;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use openxet_git::{HubCommit, Op};
use openxet_index::Oid;
use serde::Deserialize;

use crate::HubError;

/// The most operations one commit may carry (ruling M2-R4).
pub const MAX_COMMIT_OPS: usize = 25_000;

/// What a commit body may hold (ruling M2-R4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The largest decoded content of one `file` operation:
    /// `hub.lfs_threshold`, since larger files go through Xet.
    pub max_file_bytes: u64,
    /// The most operations: [`MAX_COMMIT_OPS`].
    pub max_ops: usize,
}

/// A parsed commit body: the header's fields and the operations, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRequest {
    /// The message's first line.
    pub summary: String,
    /// The rest of the message; may be empty.
    pub description: String,
    /// The branch tip the client expects, if it named one.
    pub parent: Option<Oid>,
    /// What changes.
    pub ops: Vec<Op>,
}

impl CommitRequest {
    /// The commit to make on `branch`.
    #[must_use]
    pub fn on<'a>(&'a self, branch: &'a str) -> HubCommit<'a> {
        HubCommit {
            branch,
            parent: self.parent,
            summary: &self.summary,
            description: &self.description,
            ops: self.ops.clone(),
        }
    }
}

const SHAPE_HINT: &str = "Send the commit as huggingface_hub does: NDJSON with a `header` line \
     first, then `file`, `lfsFile`, `deletedFile` or `deletedFolder` lines \
     (docs/research/S3-hub-api-shapes.md).";

/// One line: `{"key": …, "value": …}`.
#[derive(Deserialize)]
#[serde(tag = "key", content = "value", rename_all = "camelCase")]
enum Line {
    Header(Header),
    File(FileOp),
    LfsFile(LfsFileOp),
    DeletedFile(PathOp),
    DeletedFolder(PathOp),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Header {
    summary: String,
    #[serde(default)]
    description: String,
    parent_commit: Option<String>,
}

#[derive(Deserialize)]
struct FileOp {
    path: String,
    content: String,
    encoding: String,
}

#[derive(Deserialize)]
struct LfsFileOp {
    path: String,
    algo: String,
    oid: String,
    size: u64,
}

#[derive(Deserialize)]
struct PathOp {
    path: String,
}

/// Parses a commit body. Blank lines are skipped; the first other line
/// must be the `header`, and no other line may be.
///
/// # Errors
///
/// [`HubError::BadRequest`] for a body that isn't UTF-8 NDJSON of the
/// shape above, a line number naming where; for a `file` whose content
/// isn't base64 or decodes to more than `limits.max_file_bytes`; for an
/// `lfsFile` whose algorithm isn't `sha256` or whose `oid` isn't 64
/// lowercase hex characters; for a `parentCommit` that isn't a commit id;
/// and for more than `limits.max_ops` operations or none at all.
pub fn parse_commit_ndjson(body: &[u8], limits: &Limits) -> Result<CommitRequest, HubError> {
    let mut header = None;
    let mut ops = Vec::new();
    for (index, line) in body.split(|&b| b == b'\n').enumerate() {
        let number = index + 1;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let parsed: Line = serde_json::from_slice(line).map_err(|err| {
            // serde names the unknown variant or missing field without the
            // client's values for these; anything else gets a fixed text.
            let what = err.to_string();
            let what = if what.starts_with("missing field") || what.starts_with("unknown variant") {
                what.split(" at line").next().unwrap_or_default().to_owned()
            } else {
                "it is not a JSON object of the expected shape".to_owned()
            };
            bad(format!("line {number} of the commit is malformed: {what}"))
        })?;
        match (parsed, &header) {
            (Line::Header(h), None) => header = Some(h),
            (Line::Header(_), Some(_)) => {
                return Err(bad(format!("line {number} is a second header")));
            }
            (_, None) => {
                return Err(bad(format!(
                    "line {number} is an operation before the header"
                )));
            }
            (op, Some(_)) => {
                if ops.len() == limits.max_ops {
                    return Err(bad(format!(
                        "the commit carries more than {} operations",
                        limits.max_ops
                    )));
                }
                ops.push(operation(op, number, limits)?);
            }
        }
    }
    let header = header.ok_or_else(|| bad("the commit has no header line".to_owned()))?;
    if ops.is_empty() {
        return Err(bad("the commit carries no operation".to_owned()));
    }
    let parent = header
        .parent_commit
        .map(|text| {
            text.parse::<Oid>()
                .map_err(|_| bad("parentCommit is not a 40-character commit id".to_owned()))
        })
        .transpose()?;
    Ok(CommitRequest {
        summary: header.summary,
        description: header.description,
        parent,
        ops,
    })
}

fn operation(line: Line, number: usize, limits: &Limits) -> Result<Op, HubError> {
    Ok(match line {
        Line::Header(_) => unreachable!("headers are matched before operations"),
        Line::File(file) => {
            if file.encoding != "base64" {
                return Err(bad(format!(
                    "line {number}: a file's encoding must be base64"
                )));
            }
            // Base64 is 4 characters per 3 bytes: a longer text can't decode
            // within the limit, so it is refused before decoding.
            let most = limits.max_file_bytes.div_ceil(3).saturating_mul(4);
            if u64::try_from(file.content.len()).unwrap_or(u64::MAX) > most {
                return Err(too_large(number, limits));
            }
            let content = STANDARD
                .decode(file.content.as_bytes())
                .map_err(|_| bad(format!("line {number}: a file's content is not base64")))?;
            if u64::try_from(content.len()).unwrap_or(u64::MAX) > limits.max_file_bytes {
                return Err(too_large(number, limits));
            }
            Op::File {
                path: file.path,
                content: Bytes::from(content),
            }
        }
        Line::LfsFile(lfs) => {
            if lfs.algo != "sha256" {
                return Err(bad(format!(
                    "line {number}: an lfsFile's algo must be sha256"
                )));
            }
            let sha256 = sha256_from_hex(&lfs.oid).ok_or_else(|| {
                bad(format!(
                    "line {number}: an lfsFile's oid must be 64 lowercase hex characters"
                ))
            })?;
            Op::LfsFile {
                path: lfs.path,
                sha256,
                size: lfs.size,
            }
        }
        Line::DeletedFile(op) => Op::DeleteFile(op.path),
        Line::DeletedFolder(op) => Op::DeleteFolder(op.path),
    })
}

/// `text` as a SHA-256: exactly 64 lowercase hex characters, as
/// `huggingface_hub` writes it and the pointer's `oid` must be.
pub(crate) fn sha256_from_hex(text: &str) -> Option<[u8; 32]> {
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let digit = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; 32];
    for (byte, pair) in out.iter_mut().zip(bytes.chunks_exact(2)) {
        *byte = digit(pair[0])? << 4 | digit(pair[1])?;
    }
    Some(out)
}

fn too_large(number: usize, limits: &Limits) -> HubError {
    HubError::BadRequest {
        message: format!(
            "line {number}: a file larger than hub.lfs_threshold ({} bytes) can't travel inside \
             the commit",
            limits.max_file_bytes
        ),
        hint: "Upload it through Xet: preupload answers `lfs` for it, which huggingface_hub \
               follows when hf_xet is installed (`pip install hf_xet`).",
    }
}

fn bad(message: String) -> HubError {
    HubError::BadRequest {
        message,
        hint: SHAPE_HINT,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const LIMITS: Limits = Limits {
        max_file_bytes: 16,
        max_ops: 3,
    };

    fn parse(body: &str) -> Result<CommitRequest, HubError> {
        parse_commit_ndjson(body.as_bytes(), &LIMITS)
    }

    fn message(err: &HubError) -> String {
        err.to_string()
    }

    #[test]
    fn python_separators_and_every_operation() {
        let body = concat!(
            "{\"key\": \"header\", \"value\": {\"summary\": \"s\", \"description\": \"d\", ",
            "\"parentCommit\": \"0123456789abcdef0123456789abcdef01234567\"}}\n",
            "{\"key\": \"file\", \"value\": {\"content\": \"aGk=\", \"path\": \"a\", ",
            "\"encoding\": \"base64\"}}\n",
            "{\"key\": \"lfsFile\", \"value\": {\"path\": \"b\", \"algo\": \"sha256\", ",
            "\"oid\": \"0041f94a86dd643c7c0da1da750081f871464c6fd9beff4569b387d778aee7eb\", ",
            "\"size\": 209715200}}\n",
            "\n",
            "{\"key\": \"deletedFile\", \"value\": {\"path\": \"c\"}}\n",
        );
        let req = parse(body).unwrap();
        assert_eq!((req.summary.as_str(), req.description.as_str()), ("s", "d"));
        assert_eq!(
            req.parent.map(|p| p.to_string()).as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert_eq!(req.ops.len(), 3);
        assert_eq!(
            req.ops[0],
            Op::File {
                path: "a".to_owned(),
                content: Bytes::from_static(b"hi")
            }
        );
        assert!(matches!(
            &req.ops[1],
            Op::LfsFile {
                size: 209_715_200,
                ..
            }
        ));
        assert_eq!(req.ops[2], Op::DeleteFile("c".to_owned()));
        let compact = "{\"key\":\"header\",\"value\":{\"summary\":\"s\"}}\n\
                       {\"key\":\"deletedFolder\",\"value\":{\"path\":\"d\"}}";
        assert_eq!(
            parse(compact).unwrap().ops,
            [Op::DeleteFolder("d".to_owned())]
        );
    }

    #[test]
    fn refuses_what_it_cant_commit() {
        let header = "{\"key\":\"header\",\"value\":{\"summary\":\"s\"}}\n";
        let del = "{\"key\":\"deletedFile\",\"value\":{\"path\":\"x\"}}\n";
        for (body, wants) in [
            (String::new(), "no header"),
            (header.to_owned(), "no operation"),
            (del.to_owned(), "before the header"),
            (format!("{header}{header}{del}"), "second header"),
            (
                format!("{header}{}", del.repeat(4)),
                "more than 3 operations",
            ),
            (
                format!("{header}not json\n"),
                "line 2 of the commit is malformed",
            ),
            (
                format!("{header}{{\"key\":\"copyFile\",\"value\":{{}}}}\n"),
                "unknown variant",
            ),
            (
                format!(
                    "{header}{{\"key\":\"file\",\"value\":{{\"path\":\"a\",\"content\":\"*\",\
                     \"encoding\":\"base64\"}}}}\n"
                ),
                "not base64",
            ),
            (
                format!(
                    "{header}{{\"key\":\"file\",\"value\":{{\"path\":\"a\",\"content\":\"{}\",\
                     \"encoding\":\"base64\"}}}}\n",
                    STANDARD.encode([0u8; 17])
                ),
                "larger than hub.lfs_threshold",
            ),
            (
                format!(
                    "{header}{{\"key\":\"lfsFile\",\"value\":{{\"path\":\"a\",\"algo\":\"sha256\",\
                     \"oid\":\"ABC\",\"size\":1}}}}\n"
                ),
                "64 lowercase hex",
            ),
            (
                "{\"key\":\"header\",\"value\":{\"summary\":\"s\",\"parentCommit\":\"main\"}}\n"
                    .to_owned()
                    + del,
                "parentCommit",
            ),
        ] {
            let err = parse(&body).unwrap_err();
            assert!(matches!(err, HubError::BadRequest { .. }), "{body}");
            let text = message(&err);
            assert!(text.contains(wants), "{body:?}: {text}");
        }
    }

    #[test]
    fn messages_never_quote_the_body() {
        let body = "{\"key\":\"header\",\"value\":{\"summary\":\"s\"}}\n\
                    {\"key\":\"file\",\"value\":{\"path\":\"secret-zebra\",\"content\":7}}\n";
        let text = message(&parse(body).unwrap_err());
        assert!(!text.contains("zebra"), "{text}");
    }
}
