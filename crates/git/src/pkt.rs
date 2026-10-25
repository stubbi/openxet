//! pkt-line framing (git `Documentation/gitprotocol-common.txt`, "pkt-line
//! Format") and the parts of a request openxet reads before git does: a
//! protocol v2 command request (`gitprotocol-v2.txt`, "Command Request")
//! and the want section of a v0/v1 `upload-pack` request
//! (`gitprotocol-pack.txt`, "Packfile Negotiation"). Every input is
//! untrusted: nothing here panics, and every malformed request is an error.

use bytes::Bytes;
use openxet_index::Oid;

/// The longest pkt-line git sends or accepts, header included
/// (`LARGE_PACKET_MAX`).
pub const MAX_PKT_LEN: usize = 65520;

/// One pkt-line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pkt<'a> {
    /// `0000`: ends a message.
    Flush,
    /// `0001`: separates a v2 request's capabilities from its arguments.
    Delim,
    /// `0002`: ends a v2 response over a stateless transport.
    ResponseEnd,
    /// A data line, without its 4-byte header.
    Data(&'a [u8]),
}

/// A malformed request. The message goes back to the client in an `ERR`
/// line, so it quotes at most a short, escaped piece of what was sent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PktError {
    /// A header that isn't 4 hex digits, or a length of 3 or past
    /// [`MAX_PKT_LEN`].
    #[error("protocol error: bad pkt-line length \"{0}\"")]
    BadLength(String),
    /// The request ended before its closing flush.
    #[error("protocol error: the request ended before its flush packet")]
    Truncated,
    /// A v2 request without a `command=` line.
    #[error("protocol error: the request names no command")]
    MissingCommand,
    /// A v2 request with two `command=` lines.
    #[error("protocol error: the request names more than one command")]
    DuplicateCommand,
    /// A v2 capability line openxet doesn't advertise.
    #[error("protocol error: unknown capability \"{0}\"")]
    UnknownCapability(String),
    /// An `object-format` other than `sha1`.
    #[error("protocol error: object-format \"{0}\" is not supported; this server stores sha1")]
    ObjectFormat(String),
    /// A line the command doesn't take.
    #[error("protocol error: unexpected line \"{0}\"")]
    Unexpected(String),
    /// A `want` or `have` line whose object id isn't 40 hex digits.
    #[error("protocol error: expected an object id in \"{0}\"")]
    BadOid(String),
}

/// The longest piece of a client's line an error quotes, escaped.
const QUOTE_LEN: usize = 80;

/// At most [`QUOTE_LEN`] characters of `line`, escaped so no control
/// character reaches the client's terminal, with `...` if cut.
pub(crate) fn quote(line: &[u8]) -> String {
    let line = &line[..line.len().min(QUOTE_LEN * 4)];
    let mut out = String::new();
    for c in String::from_utf8_lossy(line).chars() {
        let escaped = c.escape_debug().to_string();
        if out.len() + escaped.len() > QUOTE_LEN {
            out.push_str("...");
            break;
        }
        out.push_str(&escaped);
    }
    out
}

/// Reads the pkt-line at the start of `buf`: `Ok(None)` when `buf` doesn't
/// yet hold all of it, otherwise the packet and the bytes it takes.
///
/// # Errors
///
/// [`PktError::BadLength`].
pub fn read_pkt(buf: &[u8]) -> Result<Option<(Pkt<'_>, usize)>, PktError> {
    let Some(header) = buf.get(..4) else {
        return Ok(None);
    };
    let len = header.iter().try_fold(0usize, |acc, b| {
        let digit = char::from(*b).to_digit(16)?;
        Some(acc << 4 | digit as usize)
    });
    let pkt = match len {
        Some(0) => Pkt::Flush,
        Some(1) => Pkt::Delim,
        Some(2) => Pkt::ResponseEnd,
        Some(len @ 4..=MAX_PKT_LEN) => match buf.get(4..len) {
            Some(data) => return Ok(Some((Pkt::Data(data), len))),
            None => return Ok(None),
        },
        _ => return Err(PktError::BadLength(quote(header))),
    };
    Ok(Some((pkt, 4)))
}

/// Where the first flush packet in `buf` ends, scanning from `from`, a
/// packet boundary (0, or the resume point an earlier call returned):
/// `Ok(Ok(end))` when found, `Ok(Err(resume))` when `buf` holds no flush
/// yet. A v2 request ends at its first flush; a v0/v1 want section too.
///
/// # Errors
///
/// [`PktError::BadLength`].
pub fn find_flush(buf: &[u8], from: usize) -> Result<Result<usize, usize>, PktError> {
    let mut pos = from;
    while let Some((pkt, len)) = read_pkt(buf.get(pos..).unwrap_or_default())? {
        pos += len;
        if pkt == Pkt::Flush {
            return Ok(Ok(pos));
        }
    }
    Ok(Err(pos))
}

/// The packets of `request`, which must end with its flush.
fn packets(request: &[u8]) -> impl Iterator<Item = Result<Pkt<'_>, PktError>> {
    let mut pos = 0;
    std::iter::from_fn(move || {
        let rest = request.get(pos..)?;
        if rest.is_empty() {
            return None;
        }
        Some(match read_pkt(rest) {
            Ok(Some((pkt, len))) => {
                pos += len;
                Ok(pkt)
            }
            Ok(None) => {
                pos = request.len();
                Err(PktError::Truncated)
            }
            Err(e) => {
                pos = request.len();
                Err(e)
            }
        })
    })
}

/// A data line without its trailing newline, which git strips too
/// (`PACKET_READ_CHOMP_NEWLINE`).
fn chomp(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n").unwrap_or(line)
}

fn oid(line: &[u8], hex: &[u8]) -> Result<Oid, PktError> {
    std::str::from_utf8(hex)
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| PktError::BadOid(quote(line)))
}

/// A protocol v2 command request.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum V2Command {
    /// `command=ls-refs`, which openxet answers itself.
    LsRefs(LsRefs),
    /// `command=fetch`, whose wants openxet checks before git sees them.
    Fetch(Fetch),
    /// Any other command, which never reaches git (ADR 0004 item 2).
    Other(String),
}

/// `ls-refs` arguments (`gitprotocol-v2.txt`, "ls-refs").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LsRefs {
    /// `symrefs`: name a symbolic ref's target.
    pub symrefs: bool,
    /// `peel`: name an annotated tag's peeled object.
    pub peel: bool,
    /// `unborn`: list `HEAD` even when its branch doesn't exist yet.
    pub unborn: bool,
    /// `ref-prefix` values; empty lists every ref.
    pub prefixes: Vec<Vec<u8>>,
}

/// The `fetch` arguments openxet reads; git reads the rest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fetch {
    /// `want <oid>` lines.
    pub wants: Vec<Oid>,
    /// `have <oid>` lines.
    pub haves: Vec<Oid>,
    /// Whether `done` was sent.
    pub done: bool,
    /// `want-ref <ref>` lines, which openxet refuses
    /// (`uploadpack.allowRefInWant=false`, ADR 0004 item 1).
    pub want_refs: Vec<String>,
}

/// More `ref-prefix` lines than this and git ignores them all (`ls-refs.c`,
/// `TOO_MANY_PREFIXES`); openxet does the same so its answer matches.
const TOO_MANY_PREFIXES: usize = 65536;

/// Parses one protocol v2 command request, through its closing flush.
/// Capabilities are checked against what openxet advertises (`agent`,
/// `server-option`, `object-format=sha1`); `ls-refs` arguments fully, as
/// openxet answers it; `fetch` arguments only for `want`, `have`, `done`
/// and `want-ref`, as git checks the rest.
///
/// # Errors
///
/// A [`PktError`] for a malformed request: a bad length, truncation, a
/// missing or repeated command, an unknown capability, another object
/// format, an unexpected `ls-refs` line or packet, a bad object id, or
/// bytes after the flush.
pub fn parse_v2_command(pkt: &[u8]) -> Result<V2Command, PktError> {
    let mut command: Option<String> = None;
    let mut args: Vec<&[u8]> = Vec::new();
    let mut in_args = false;
    let mut ended = false;
    for packet in packets(pkt) {
        if ended {
            return Err(PktError::Unexpected(
                "data after the flush packet".to_owned(),
            ));
        }
        match packet? {
            Pkt::Flush => ended = true,
            Pkt::Delim if !in_args => in_args = true,
            Pkt::Delim | Pkt::ResponseEnd => {
                return Err(PktError::Unexpected("a delimiter packet".to_owned()));
            }
            Pkt::Data(line) if in_args => args.push(chomp(line)),
            Pkt::Data(line) => {
                let line = chomp(line);
                if let Some(name) = line.strip_prefix(b"command=") {
                    if command.is_some() {
                        return Err(PktError::DuplicateCommand);
                    }
                    command = Some(String::from_utf8_lossy(name).into_owned());
                } else {
                    capability(line)?;
                }
            }
        }
    }
    if !ended {
        return Err(PktError::Truncated);
    }
    match command.as_deref() {
        None => Err(PktError::MissingCommand),
        Some("ls-refs") => ls_refs(&args).map(V2Command::LsRefs),
        Some("fetch") => fetch(&args).map(V2Command::Fetch),
        Some(_) => Ok(V2Command::Other(command.unwrap_or_default())),
    }
}

/// A client capability line: only what openxet advertises besides the
/// commands (git `serve.c` refuses an unadvertised one too).
fn capability(line: &[u8]) -> Result<(), PktError> {
    let (key, value) = match line.iter().position(|b| *b == b'=') {
        Some(i) => (&line[..i], Some(&line[i + 1..])),
        None => (line, None),
    };
    match (key, value) {
        (b"agent" | b"server-option", _) | (b"object-format", Some(b"sha1")) => Ok(()),
        (b"object-format", value) => Err(PktError::ObjectFormat(quote(value.unwrap_or_default()))),
        _ => Err(PktError::UnknownCapability(quote(line))),
    }
}

fn ls_refs(args: &[&[u8]]) -> Result<LsRefs, PktError> {
    let mut out = LsRefs::default();
    let mut too_many = false;
    for arg in args {
        match *arg {
            b"symrefs" => out.symrefs = true,
            b"peel" => out.peel = true,
            b"unborn" => out.unborn = true,
            _ => match arg.strip_prefix(b"ref-prefix ") {
                Some(prefix) if out.prefixes.len() < TOO_MANY_PREFIXES => {
                    out.prefixes.push(prefix.to_vec());
                }
                Some(_) => too_many = true,
                None => return Err(PktError::Unexpected(quote(arg))),
            },
        }
    }
    if too_many {
        out.prefixes.clear();
    }
    Ok(out)
}

fn fetch(args: &[&[u8]]) -> Result<Fetch, PktError> {
    let mut out = Fetch::default();
    for arg in args {
        if let Some(hex) = arg.strip_prefix(b"want ") {
            out.wants.push(oid(arg, hex)?);
        } else if let Some(hex) = arg.strip_prefix(b"have ") {
            out.haves.push(oid(arg, hex)?);
        } else if let Some(name) = arg.strip_prefix(b"want-ref ") {
            out.want_refs
                .push(String::from_utf8_lossy(name).into_owned());
        } else if *arg == b"done" {
            out.done = true;
        }
    }
    Ok(out)
}

/// The wants of a v0/v1 `upload-pack` request's want section, through its
/// closing flush: `want <oid>` lines, the first followed by the client's
/// capabilities after a space. Other lines (`shallow`, `deepen`, ...) are
/// left to git.
///
/// # Errors
///
/// [`PktError::Truncated`], [`PktError::BadLength`],
/// [`PktError::Unexpected`] for a delimiter packet, and
/// [`PktError::BadOid`].
pub fn parse_v0_wants(section: &[u8]) -> Result<Vec<Oid>, PktError> {
    let mut wants = Vec::new();
    for packet in packets(section) {
        match packet? {
            Pkt::Flush => return Ok(wants),
            Pkt::Data(line) => {
                let line = chomp(line);
                if let Some(rest) = line.strip_prefix(b"want ") {
                    let hex = rest.split(|b| *b == b' ').next().unwrap_or_default();
                    wants.push(oid(line, hex)?);
                }
            }
            Pkt::Delim | Pkt::ResponseEnd => {
                return Err(PktError::Unexpected("a delimiter packet".to_owned()));
            }
        }
    }
    Err(PktError::Truncated)
}

/// Appends `data` as one pkt-line. `data` is at most [`MAX_PKT_LEN`] − 4
/// bytes; openxet only frames its own short lines.
pub fn write_pkt(out: &mut Vec<u8>, data: &[u8]) {
    let len = (data.len() + 4).min(MAX_PKT_LEN);
    out.extend_from_slice(format!("{len:04x}").as_bytes());
    out.extend_from_slice(&data[..len - 4]);
}

/// `0000`.
pub const FLUSH: &[u8] = b"0000";

/// An `ERR` packet carrying `message` (`gitprotocol-pack.txt`: the client
/// reports it as `remote error: {message}`).
#[must_use]
pub fn err_pkt(message: &str) -> Bytes {
    let mut out = Vec::with_capacity(message.len() + 9);
    write_pkt(&mut out, format!("ERR {message}\n").as_bytes());
    Bytes::from(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(lines: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for line in lines {
            match *line {
                "0000" => out.extend_from_slice(b"0000"),
                "0001" => out.extend_from_slice(b"0001"),
                l => write_pkt(&mut out, format!("{l}\n").as_bytes()),
            }
        }
        out
    }

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    #[test]
    fn pkt_lengths() {
        assert_eq!(read_pkt(b"000"), Ok(None));
        assert_eq!(read_pkt(b"0000rest"), Ok(Some((Pkt::Flush, 4))));
        assert_eq!(read_pkt(b"0001"), Ok(Some((Pkt::Delim, 4))));
        assert_eq!(read_pkt(b"0002"), Ok(Some((Pkt::ResponseEnd, 4))));
        assert_eq!(read_pkt(b"0004"), Ok(Some((Pkt::Data(b""), 4))));
        assert_eq!(read_pkt(b"0006a"), Ok(None));
        assert_eq!(read_pkt(b"0006ab"), Ok(Some((Pkt::Data(b"ab"), 6))));
        assert_eq!(
            read_pkt(b"000Aabcdef"),
            Ok(Some((Pkt::Data(b"abcdef"), 10)))
        );
        for bad in [&b"0003"[..], b"fff1", b"zzzz", b"+001", b"\xff000"] {
            assert!(
                matches!(read_pkt(bad), Err(PktError::BadLength(_))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn flush_found_across_calls() {
        let full = req(&["want x", "0000", "have y"]);
        let cut = &full[..7];
        let resume = match find_flush(cut, 0) {
            Ok(Err(resume)) => resume,
            other => panic!("{other:?}"),
        };
        assert_eq!(resume, 0);
        assert_eq!(find_flush(&full, resume), Ok(Ok(15)));
    }

    #[test]
    fn v2_commands() {
        let ls = req(&[
            "command=ls-refs",
            "agent=git/2.39.5",
            "object-format=sha1",
            "0001",
            "peel",
            "symrefs",
            "unborn",
            "ref-prefix HEAD",
            "ref-prefix refs/heads/",
            "0000",
        ]);
        assert_eq!(
            parse_v2_command(&ls),
            Ok(V2Command::LsRefs(LsRefs {
                symrefs: true,
                peel: true,
                unborn: true,
                prefixes: vec![b"HEAD".to_vec(), b"refs/heads/".to_vec()],
            }))
        );
        let fetch = req(&[
            "command=fetch",
            "0001",
            "thin-pack",
            &format!("want {A}"),
            &format!("have {B}"),
            "want-ref refs/heads/main",
            "done",
            "0000",
        ]);
        let Ok(V2Command::Fetch(f)) = parse_v2_command(&fetch) else {
            panic!("not a fetch");
        };
        assert_eq!(f.wants, vec![ok_oid(A)]);
        assert_eq!(f.haves, vec![ok_oid(B)]);
        assert!(f.done);
        assert_eq!(f.want_refs, vec!["refs/heads/main".to_owned()]);
        assert_eq!(
            parse_v2_command(&req(&["command=object-info", "0001", "size", "0000"])),
            Ok(V2Command::Other("object-info".to_owned()))
        );
    }

    #[test]
    fn v2_malformed() {
        let cases: &[(&[&str], PktError)] = &[
            (&["0000"], PktError::MissingCommand),
            (&["command=fetch"], PktError::Truncated),
            (
                &["command=fetch", "command=ls-refs", "0000"],
                PktError::DuplicateCommand,
            ),
            (
                &["command=ls-refs", "session-id=x", "0000"],
                PktError::UnknownCapability("session-id=x".to_owned()),
            ),
            (
                &["command=ls-refs", "object-format=sha256", "0000"],
                PktError::ObjectFormat("sha256".to_owned()),
            ),
            (
                &["command=ls-refs", "0001", "nonsense", "0000"],
                PktError::Unexpected("nonsense".to_owned()),
            ),
            (
                &["command=fetch", "0001", "want 12", "0000"],
                PktError::BadOid("want 12".to_owned()),
            ),
            (
                &["command=fetch", "0001", "0001", "0000"],
                PktError::Unexpected("a delimiter packet".to_owned()),
            ),
            (
                &["command=fetch", "0000", "command=fetch", "0000"],
                PktError::Unexpected("data after the flush packet".to_owned()),
            ),
        ];
        for (lines, err) in cases {
            assert_eq!(
                parse_v2_command(&req(lines)).as_ref(),
                Err(err),
                "{lines:?}"
            );
        }
    }

    #[test]
    fn too_many_prefixes_means_all() {
        let mut lines = vec!["command=ls-refs".to_owned(), "0001".to_owned()];
        lines.extend((0..=TOO_MANY_PREFIXES).map(|i| format!("ref-prefix refs/{i}")));
        lines.push("0000".to_owned());
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let Ok(V2Command::LsRefs(ls)) = parse_v2_command(&req(&lines)) else {
            panic!("not ls-refs");
        };
        assert!(ls.prefixes.is_empty());
    }

    #[test]
    fn v0_wants() {
        let section = req(&[
            &format!("want {A} multi_ack_detailed side-band-64k agent=git/2.39.5"),
            &format!("want {B}"),
            "deepen 1",
            "0000",
        ]);
        assert_eq!(parse_v0_wants(&section), Ok(vec![ok_oid(A), ok_oid(B)]));
        assert_eq!(parse_v0_wants(&req(&["0000"])), Ok(vec![]));
        assert_eq!(
            parse_v0_wants(&req(&[&format!("want {A}")])),
            Err(PktError::Truncated)
        );
        assert!(matches!(
            parse_v0_wants(&req(&["want refs/heads/main", "0000"])),
            Err(PktError::BadOid(_))
        ));
    }

    #[test]
    fn quotes_are_short_and_escaped() {
        assert_eq!(quote(b"want x"), "want x");
        let control = quote(&[0x1f; 200]);
        assert!(
            control.len() <= QUOTE_LEN + 3 && control.ends_with("..."),
            "{control}"
        );
        assert!(!control.contains('\x1f'));
        let err = PktError::Unexpected(quote(&[0x1f; 200])).to_string();
        assert!(err.len() < 200, "{err}");
    }

    #[test]
    fn err_packets() {
        assert_eq!(&err_pkt("x")[..], b"000aERR x\n");
        let long = "y".repeat(MAX_PKT_LEN);
        assert_eq!(err_pkt(&long).len(), MAX_PKT_LEN);
    }

    fn ok_oid(hex: &str) -> Oid {
        hex.parse().unwrap_or_else(|e| panic!("{e}"))
    }
}
