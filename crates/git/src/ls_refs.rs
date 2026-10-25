//! What openxet says about refs and itself before any fetch (ADR 0004
//! item 2): the v2 capability advertisement filtered to the allowlist, the
//! `agent` of every advertisement, and `ls-refs` answered from the
//! snapshot.

use std::fmt::Write as _;

use openxet_index::Oid;

use crate::pkt::{FLUSH, LsRefs, Pkt, read_pkt, write_pkt};
use crate::reach;
use crate::snapshot::HEAD_REF;
use crate::{GitError, SnapshotRepo};

/// The `agent` capability openxet advertises in place of git's, which would
/// disclose the server's git version; clients only log it.
pub(crate) const AGENT: &str = concat!("agent=openxet/", env!("CARGO_PKG_VERSION"));

/// git's v0/v1 ref advertisement with the `agent` capability of its first
/// ref line (the capability list after the NUL) replaced by [`AGENT`].
/// Everything else passes through byte for byte.
pub(crate) fn with_openxet_agent(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut pos = 0;
    let mut replaced = false;
    while let Ok(Some((pkt, len))) = read_pkt(raw.get(pos..).unwrap_or_default()) {
        let packet = &raw[pos..pos + len];
        pos += len;
        let line = match pkt {
            Pkt::Data(line) if !replaced => line,
            _ => {
                out.extend_from_slice(packet);
                continue;
            }
        };
        let Some(nul) = line.iter().position(|b| *b == 0) else {
            out.extend_from_slice(packet);
            continue;
        };
        replaced = true;
        let (caps, newline) = match line[nul + 1..].strip_suffix(b"\n") {
            Some(caps) => (caps, true),
            None => (&line[nul + 1..], false),
        };
        let mut rewritten = line[..=nul].to_vec();
        let caps: Vec<&[u8]> = caps
            .split(|b| *b == b' ')
            .map(|cap| {
                if cap.starts_with(b"agent=") {
                    AGENT.as_bytes()
                } else {
                    cap
                }
            })
            .collect();
        rewritten.extend_from_slice(&caps.join(&b' '));
        if newline {
            rewritten.push(b'\n');
        }
        write_pkt(&mut out, &rewritten);
    }
    out.extend_from_slice(raw.get(pos..).unwrap_or_default());
    out
}

/// git's v2 capability advertisement reduced to ADR 0004 item 2's
/// allowlist: `version 2`, `agent` (as [`AGENT`]), `ls-refs`, `fetch` with
/// `filter` removed from its features, `server-option` and
/// `object-format`. Adding a line needs an ADR amendment.
pub(crate) fn filter_v2_advertisement(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Ok(Some((pkt, len))) = read_pkt(raw.get(pos..).unwrap_or_default()) {
        pos += len;
        let line = match pkt {
            Pkt::Data(line) => line.strip_suffix(b"\n").unwrap_or(line),
            Pkt::Flush => break,
            Pkt::Delim | Pkt::ResponseEnd => continue,
        };
        let (key, value) = match line.iter().position(|b| *b == b'=') {
            Some(i) => (&line[..i], Some(&line[i + 1..])),
            None => (line, None),
        };
        let kept = match (key, value) {
            (b"agent", _) => AGENT.as_bytes().to_vec(),
            (b"version 2", None) | (b"ls-refs" | b"server-option" | b"object-format", _) => {
                line.to_vec()
            }
            (b"fetch", None) => b"fetch".to_vec(),
            (b"fetch", Some(features)) => {
                let features: Vec<&[u8]> = features
                    .split(|b| *b == b' ')
                    .filter(|f| !f.is_empty() && *f != b"filter")
                    .collect();
                let mut kept = b"fetch".to_vec();
                if !features.is_empty() {
                    kept.push(b'=');
                    kept.extend_from_slice(&features.join(&b' '));
                }
                kept
            }
            _ => continue,
        };
        let mut kept = kept;
        kept.push(b'\n');
        write_pkt(&mut out, &kept);
    }
    out.extend_from_slice(FLUSH);
    out
}

/// `ls-refs` from the snapshot's refs (git `ls-refs.c`, which it matches
/// byte for byte): `HEAD` first, when it matches a prefix, naming
/// [`HEAD_REF`]; then every ref matching a prefix, in name order. `HEAD`
/// whose branch doesn't exist is listed as `unborn` only when the client
/// asked for both `unborn` and `symrefs`.
pub(crate) fn ls_refs(snapshot: &SnapshotRepo, args: &LsRefs) -> Result<Vec<u8>, GitError> {
    let repo = if args.peel {
        Some(reach::open(snapshot.dir.path()).map_err(GitError::Objects)?)
    } else {
        None
    };
    let matches =
        |name: &[u8]| args.prefixes.is_empty() || args.prefixes.iter().any(|p| name.starts_with(p));
    let line = |out: &mut Vec<u8>, oid: Option<Oid>, name: &str, symref: bool| {
        let mut text = match oid {
            Some(oid) => format!("{oid} {name}"),
            None => format!("unborn {name}"),
        };
        if symref && args.symrefs {
            text.push_str(" symref-target:");
            text.push_str(HEAD_REF);
        }
        if let (Some(repo), Some(oid)) = (&repo, oid) {
            let (chain, end) = reach::peel(repo, oid);
            if let Some(peeled) = chain.last().filter(|_| chain.len() > 1 && end.is_some()) {
                let _ = write!(text, " peeled:{peeled}");
            }
        }
        text.push('\n');
        write_pkt(out, text.as_bytes());
    };
    let mut out = Vec::new();
    if matches(b"HEAD") {
        let head = snapshot
            .refs
            .iter()
            .find(|(name, _)| name == HEAD_REF)
            .map(|(_, oid)| *oid);
        if head.is_some() || (args.unborn && args.symrefs) {
            line(&mut out, head, "HEAD", true);
        }
    }
    for (name, oid) in &snapshot.refs {
        if matches(name.as_bytes()) {
            line(&mut out, Some(*oid), name, false);
        }
    }
    out.extend_from_slice(FLUSH);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{AGENT, filter_v2_advertisement, with_openxet_agent};
    use crate::pkt::write_pkt;

    fn pkts(lines: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for line in lines {
            write_pkt(&mut out, format!("{line}\n").as_bytes());
        }
        out.extend_from_slice(b"0000");
        out
    }

    #[test]
    fn advertisement_filtered_to_the_allowlist() {
        let raw = pkts(&[
            "version 2",
            "agent=git/2.39.5",
            "ls-refs=unborn",
            "fetch=shallow wait-for-done filter",
            "server-option",
            "object-format=sha1",
            "object-info",
            "bundle-uri",
            "session-id=abc",
            "promisor-remote=x",
        ]);
        assert_eq!(
            filter_v2_advertisement(&raw),
            pkts(&[
                "version 2",
                AGENT,
                "ls-refs=unborn",
                "fetch=shallow wait-for-done",
                "server-option",
                "object-format=sha1",
            ])
        );
        assert_eq!(
            filter_v2_advertisement(&pkts(&["version 2", "fetch=filter", "ls-refs"])),
            pkts(&["version 2", "fetch", "ls-refs"])
        );
    }

    #[test]
    fn v0_agent_replaced() {
        let oid = "1111111111111111111111111111111111111111";
        let mut raw = Vec::new();
        write_pkt(
            &mut raw,
            format!("{oid} HEAD\0multi_ack thin-pack symref=HEAD:refs/heads/main agent=git/2.39.5 object-format=sha1\n").as_bytes(),
        );
        write_pkt(
            &mut raw,
            format!("{oid} refs/heads/main\0agent=x\n").as_bytes(),
        );
        raw.extend_from_slice(b"0000");
        let mut want = Vec::new();
        write_pkt(
            &mut want,
            format!("{oid} HEAD\0multi_ack thin-pack symref=HEAD:refs/heads/main {AGENT} object-format=sha1\n").as_bytes(),
        );
        write_pkt(
            &mut want,
            format!("{oid} refs/heads/main\0agent=x\n").as_bytes(),
        );
        want.extend_from_slice(b"0000");
        assert_eq!(
            with_openxet_agent(&raw),
            want,
            "only the first capability line changes"
        );
        let v1 = [b"000eversion 1\n".as_slice(), &raw].concat();
        let want_v1 = [b"000eversion 1\n".as_slice(), &want].concat();
        assert_eq!(with_openxet_agent(&v1), want_v1);
        assert_eq!(with_openxet_agent(b"0000garbage"), b"0000garbage");
    }
}
