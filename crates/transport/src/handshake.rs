//! The handshake's lines (spec/transport.md, "Handshake").
//!
//! Lines are UTF-8, end in `\n`, and hold at most 1,024 bytes with the newline.
//! Fields are separated by one space; numbers are lower-case hex without a
//! prefix.

use core::fmt;

/// The longest line, the newline included.
pub const MAX_LINE: usize = 1024;
/// The protocol version in `HELLO`.
pub const VERSION: u64 = 1;

/// A line from either side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    /// `HELLO 1 <token> <sidecar_boot>`, from the sidecar.
    Hello {
        /// The protocol version.
        version: u64,
        /// The token, as sent.
        token: String,
        /// The sidecar's boot.
        sidecar_boot: u64,
    },
    /// `OK <layout_hash> <agent_boot> <attach_id> <path>`, from the agent.
    Ok {
        /// The layout hash.
        layout_hash: u64,
        /// The agent's boot.
        agent_boot: u64,
        /// The attach ID.
        attach_id: u64,
        /// The file's path as the agent opened it.
        path: String,
    },
    /// `ERR <reason>`, from the agent.
    Err(Refusal),
}

/// Why the agent refused a `HELLO`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The protocol version is not one the agent speaks.
    Version,
    /// The token does not match.
    Token,
    /// The kill switch is on.
    Disabled,
    /// The agent has no mapped file.
    NoFile,
}

impl Refusal {
    /// The reason as the line carries it.
    pub fn as_str(self) -> &'static str {
        match self {
            Refusal::Version => "version",
            Refusal::Token => "token",
            Refusal::Disabled => "disabled",
            Refusal::NoFile => "no_file",
        }
    }
}

/// A line that does not parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineError(pub &'static str);

impl fmt::Display for LineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for LineError {}

fn hex(field: &str) -> Result<u64, LineError> {
    let ok = !field.is_empty()
        && field.len() <= 16
        && field
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !ok {
        return Err(LineError("a number that is not lower-case hex"));
    }
    u64::from_str_radix(field, 16).map_err(|_| LineError("a number that is not lower-case hex"))
}

impl Line {
    /// The line, with its newline. `None` when it would be over [`MAX_LINE`]
    /// or a field holds a space or a newline where it may not.
    pub fn encode(&self) -> Option<String> {
        let s = match self {
            Line::Hello {
                version,
                token,
                sidecar_boot,
            } => {
                if token.is_empty() || token.contains([' ', '\n']) {
                    return None;
                }
                format!("HELLO {version:x} {token} {sidecar_boot:x}\n")
            }
            Line::Ok {
                layout_hash,
                agent_boot,
                attach_id,
                path,
            } => {
                if path.contains('\n') {
                    return None;
                }
                format!("OK {layout_hash:x} {agent_boot:x} {attach_id:x} {path}\n")
            }
            Line::Err(r) => format!("ERR {}\n", r.as_str()),
        };
        (s.len() <= MAX_LINE).then_some(s)
    }

    /// Parse one line, with or without its newline.
    pub fn parse(line: &[u8]) -> Result<Line, LineError> {
        if line.len() > MAX_LINE {
            return Err(LineError("a line over 1,024 bytes"));
        }
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let text = core::str::from_utf8(line).map_err(|_| LineError("a line that is not UTF-8"))?;
        if text.contains('\n') {
            return Err(LineError("two lines"));
        }
        let (word, rest) = text.split_once(' ').unwrap_or((text, ""));
        match word {
            "HELLO" => {
                let f: Vec<&str> = rest.split(' ').collect();
                let [version, token, boot] = f[..] else {
                    return Err(LineError("HELLO takes three fields"));
                };
                if token.is_empty() {
                    return Err(LineError("an empty token"));
                }
                Ok(Line::Hello {
                    version: hex(version)?,
                    token: token.to_owned(),
                    sidecar_boot: hex(boot)?,
                })
            }
            "OK" => {
                let mut f = rest.splitn(4, ' ');
                let (Some(h), Some(b), Some(a), Some(path)) =
                    (f.next(), f.next(), f.next(), f.next())
                else {
                    return Err(LineError("OK takes four fields"));
                };
                if path.is_empty() {
                    return Err(LineError("an empty path"));
                }
                Ok(Line::Ok {
                    layout_hash: hex(h)?,
                    agent_boot: hex(b)?,
                    attach_id: hex(a)?,
                    path: path.to_owned(),
                })
            }
            "ERR" => Ok(Line::Err(match rest {
                "version" => Refusal::Version,
                "token" => Refusal::Token,
                "disabled" => Refusal::Disabled,
                "no_file" => Refusal::NoFile,
                _ => return Err(LineError("an unknown ERR reason")),
            })),
            _ => Err(LineError("an unknown keyword")),
        }
    }
}

/// Whether two tokens are equal, in time that depends only on their lengths.
pub fn tokens_equal(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y));
    core::hint::black_box(diff) == 0
}

/// Collects bytes from a socket into lines.
#[derive(Clone, Debug, Default)]
pub struct LineBuffer {
    buf: Vec<u8>,
}

impl LineBuffer {
    /// An empty buffer.
    pub fn new() -> LineBuffer {
        LineBuffer::default()
    }

    /// Add bytes read from the socket.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Take the first complete line, its newline included. An error when more
    /// than [`MAX_LINE`] bytes came with no newline among them.
    pub fn line(&mut self) -> Result<Option<Vec<u8>>, LineError> {
        match self.buf.iter().position(|&b| b == b'\n') {
            Some(i) if i < MAX_LINE => Ok(Some(self.buf.drain(..=i).collect())),
            Some(_) => Err(LineError("a line over 1,024 bytes")),
            None if self.buf.len() >= MAX_LINE => Err(LineError("a line over 1,024 bytes")),
            None => Ok(None),
        }
    }

    /// Bytes held that are not yet a line.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether nothing is held.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_round_trip() {
        for l in [
            Line::Hello {
                version: 1,
                token: "ab".repeat(32),
                sidecar_boot: 0xdead,
            },
            Line::Ok {
                layout_hash: 0x501dada1eb913155,
                agent_boot: 1,
                attach_id: 2,
                path: "C:\\Users\\a b\\nanoprobe.map".into(),
            },
            Line::Err(Refusal::NoFile),
        ] {
            let s = l.encode().expect("fits");
            assert!(s.ends_with('\n'));
            assert_eq!(Line::parse(s.as_bytes()), Ok(l));
        }
    }

    #[test]
    fn bad_lines_are_refused() {
        for bad in [
            &b"HELLO 1 tok"[..],
            b"HELLO 1  ff",
            b"HELLO 0x1 t ff",
            b"HELLO 1 t FF",
            b"HELLO 1 t 11112222333344445",
            b"OK 1 2 3",
            b"OK 1 2 3 ",
            b"ERR maybe",
            b"HI",
            b"OK 1 2 3 p\nx",
            b"\xff",
        ] {
            assert!(
                Line::parse(bad).is_err(),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
        assert!(Line::parse(&[b'E'; 1025]).is_err());
        let long = Line::Ok {
            layout_hash: 1,
            agent_boot: 1,
            attach_id: 1,
            path: "p".repeat(1020),
        };
        assert_eq!(long.encode(), None);
    }

    #[test]
    fn the_buffer_splits_lines_and_bounds_them() {
        let mut b = LineBuffer::new();
        b.push(b"ERR tok");
        assert_eq!(b.line(), Ok(None));
        b.push(b"en\nOK");
        assert_eq!(b.line(), Ok(Some(b"ERR token\n".to_vec())));
        assert_eq!(b.len(), 2);
        let mut b = LineBuffer::new();
        b.push(&[b'x'; 1024]);
        assert!(b.line().is_err());
    }

    #[test]
    fn tokens_compare_whole() {
        assert!(tokens_equal(b"abc", b"abc"));
        assert!(!tokens_equal(b"abc", b"abd"));
        assert!(!tokens_equal(b"abc", b"ab"));
    }
}
