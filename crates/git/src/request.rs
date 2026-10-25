//! Measuring a receive-pack request as it streams to git (ADR 0004 item 3:
//! openxet is the transport, so it counts the client's pack bytes and reads
//! its header's object count, which split the client's objects from the
//! bases `index-pack --fix-thin` appends). The bytes are untrusted; git
//! parses them for real, and a request this parser can't follow yields no
//! pack, which fails the push closed.

/// What openxet learns about the client's pack while streaming it to git.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PackStats {
    /// The object count in the client's pack header, if a pack came.
    pub header_count: Option<u32>,
    /// The client's pack length, header to trailer.
    pub pack_bytes: u64,
}

/// An incremental parser of a receive-pack request (gitprotocol-pack(5),
/// "Reference Update Request and Packfile Transfer"): pkt-lines of shallow
/// lines and commands up to a flush, push options up to a flush when the
/// first command asks for them, then the raw pack.
#[derive(Debug, Default)]
pub struct RequestParser {
    state: ParseState,
    buf: Vec<u8>,
    push_options: bool,
    first_command_seen: bool,
    pack_bytes: u64,
    header: Vec<u8>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum ParseState {
    #[default]
    Commands,
    PushOptions,
    Pack,
    Malformed,
}

impl RequestParser {
    /// Takes the next bytes of the request.
    pub fn push(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            match self.state {
                ParseState::Malformed => return,
                ParseState::Pack => {
                    let need = 12usize.saturating_sub(self.header.len()).min(data.len());
                    self.header.extend_from_slice(&data[..need]);
                    self.pack_bytes += data.len() as u64;
                    return;
                }
                ParseState::Commands | ParseState::PushOptions => {
                    let want = if self.buf.len() < 4 {
                        4 - self.buf.len()
                    } else {
                        self.pkt_len()
                            .map_or(0, |n| n.saturating_sub(self.buf.len()))
                    };
                    let take = want.min(data.len());
                    self.buf.extend_from_slice(&data[..take]);
                    data = &data[take..];
                    self.advance();
                }
            }
        }
    }

    fn pkt_len(&self) -> Option<usize> {
        let text = std::str::from_utf8(self.buf.get(..4)?).ok()?;
        usize::from_str_radix(text, 16).ok()
    }

    fn advance(&mut self) {
        if self.buf.len() < 4 {
            return;
        }
        match self.pkt_len() {
            Some(0) => {
                self.buf.clear();
                self.state = match self.state {
                    ParseState::Commands if self.push_options => ParseState::PushOptions,
                    _ => ParseState::Pack,
                };
            }
            Some(n) if (5..=65520).contains(&n) => {
                if self.buf.len() == n {
                    if self.state == ParseState::Commands && !self.first_command_seen {
                        let line = &self.buf[4..];
                        if !line.starts_with(b"shallow ") {
                            self.first_command_seen = true;
                            if let Some(nul) = line.iter().position(|b| *b == 0) {
                                self.push_options = line[nul + 1..]
                                    .split(|b| *b == b' ' || *b == b'\n')
                                    .any(|cap| cap == b"push-options");
                            }
                        }
                    }
                    self.buf.clear();
                }
            }
            _ => self.state = ParseState::Malformed,
        }
    }

    /// What the request held, once it has ended.
    #[must_use]
    pub fn finish(self) -> PackStats {
        let header_count = (self.state == ParseState::Pack
            && self.header.len() == 12
            && self.header.starts_with(b"PACK"))
        .then(|| {
            u32::from_be_bytes([
                self.header[8],
                self.header[9],
                self.header[10],
                self.header[11],
            ])
        });
        PackStats {
            header_count,
            pack_bytes: if header_count.is_some() {
                self.pack_bytes
            } else {
                0
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RequestParser;

    fn pkt(payload: &[u8]) -> Vec<u8> {
        let mut out = format!("{:04x}", payload.len() + 4).into_bytes();
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn parser_measures_the_pack_across_chunks() {
        let zero = "0".repeat(40);
        let a = "a".repeat(40);
        let mut req =
            pkt(format!("{zero} {a} refs/heads/main\0report-status push-options").as_bytes());
        req.extend_from_slice(b"0000");
        req.extend(pkt(b"an option"));
        req.extend_from_slice(b"0000");
        let mut pack = b"PACK\0\0\0\x02\0\0\0\x07".to_vec();
        pack.extend_from_slice(&[9; 100]);
        req.extend_from_slice(&pack);
        for split in [1, 3, 7, 4096] {
            let mut p = RequestParser::default();
            for chunk in req.chunks(split) {
                p.push(chunk);
            }
            let stats = p.finish();
            assert_eq!(stats.header_count, Some(7), "split {split}");
            assert_eq!(stats.pack_bytes, pack.len() as u64, "split {split}");
        }
    }

    #[test]
    fn parser_without_pack_or_malformed() {
        let zero = "0".repeat(40);
        let a = "a".repeat(40);
        let mut req = pkt(format!("{a} {zero} refs/heads/x\0report-status").as_bytes());
        req.extend_from_slice(b"0000");
        let mut p = RequestParser::default();
        p.push(&req);
        assert_eq!(p.finish().header_count, None);
        let mut p = RequestParser::default();
        p.push(b"zzzzPACK\0\0\0\x02\0\0\0\x01");
        assert_eq!(p.finish().header_count, None);
    }
}
