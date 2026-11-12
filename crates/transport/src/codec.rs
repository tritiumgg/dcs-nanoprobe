//! Payload encodings: little-endian fixed fields, `str` and `lv`
//! (spec/transport.md, "Encodings").

use core::fmt;

use crate::layout::lv_tag;

/// The deepest a table may nest.
pub const MAX_DEPTH: usize = 16;
/// The most pairs one table may hold.
pub const MAX_PAIRS: u32 = 65_536;
/// The longest `str`, in bytes.
pub const MAX_STR: usize = u16::MAX as usize;

/// A Lua value as the `lv` encoding carries it.
#[derive(Clone, Debug, PartialEq)]
pub enum Lv {
    /// nil
    Nil,
    /// A boolean.
    Bool(bool),
    /// A number.
    Number(f64),
    /// A string: any bytes.
    String(Vec<u8>),
    /// A table, as its pairs in order.
    Table(Vec<(Lv, Lv)>),
    /// The type name of a value that cannot be carried.
    Other(String),
}

/// A payload that does not decode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeError {
    /// Where in the payload.
    pub at: usize,
    /// What was wrong.
    pub what: &'static str,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "at byte {}: {}", self.at, self.what)
    }
}

impl std::error::Error for DecodeError {}

/// Builds a payload.
#[derive(Clone, Debug, Default)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    /// An empty payload.
    pub fn new() -> Writer {
        Writer::default()
    }

    /// The payload.
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }

    /// Bytes so far.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether nothing is written.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// A `u8`.
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }
    /// A `u16`.
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    /// A `u32`.
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    /// A `u64`.
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    /// An `f32`.
    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    /// An `f64`.
    pub fn f64(&mut self, v: f64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    /// `n` zero bytes.
    pub fn pad(&mut self, n: usize) -> &mut Self {
        self.buf.resize(self.buf.len() + n, 0);
        self
    }

    /// A `str` of any bytes: each invalid UTF-8 sequence becomes U+FFFD, and
    /// the result is cut at a character boundary to at most `max` bytes, which
    /// is itself at most 65,535.
    pub fn str_cut(&mut self, bytes: &[u8], max: usize) -> &mut Self {
        let text = String::from_utf8_lossy(bytes);
        let mut end = text.len().min(max.min(MAX_STR));
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.u16(end as u16);
        self.buf.extend_from_slice(&text.as_bytes()[..end]);
        self
    }

    /// A `str`, cut to 65,535 bytes.
    pub fn str(&mut self, text: &str) -> &mut Self {
        self.str_cut(text.as_bytes(), MAX_STR)
    }

    /// An `lv` value. Nesting past [`MAX_DEPTH`] or a table over
    /// [`MAX_PAIRS`] is written as nil, so the result always decodes.
    pub fn lv(&mut self, v: &Lv) -> &mut Self {
        self.lv_at(v, 0);
        self
    }

    fn lv_at(&mut self, v: &Lv, depth: usize) {
        match v {
            Lv::Nil => self.tag(lv_tag::NIL),
            Lv::Bool(false) => self.tag(lv_tag::FALSE),
            Lv::Bool(true) => self.tag(lv_tag::TRUE),
            Lv::Number(n) => {
                self.tag(lv_tag::NUMBER);
                self.f64(*n);
            }
            Lv::String(s) => {
                self.tag(lv_tag::STRING);
                self.u32(s.len() as u32);
                self.buf.extend_from_slice(s);
            }
            Lv::Table(pairs) => {
                if depth >= MAX_DEPTH || pairs.len() > MAX_PAIRS as usize {
                    return self.tag(lv_tag::NIL);
                }
                self.tag(lv_tag::TABLE);
                let count_at = self.buf.len();
                self.u32(0);
                let mut n = 0u32;
                for (k, v) in pairs {
                    if matches!(k, Lv::Nil) || matches!(k, Lv::Number(x) if x.is_nan()) {
                        continue;
                    }
                    self.lv_at(k, depth + 1);
                    self.lv_at(v, depth + 1);
                    n += 1;
                }
                self.buf[count_at..count_at + 4].copy_from_slice(&n.to_le_bytes());
            }
            Lv::Other(name) => {
                self.tag(lv_tag::OTHER);
                self.str(name);
            }
        }
    }

    fn tag(&mut self, t: u64) {
        self.buf.push(t as u8);
    }
}

/// Reads a payload, checking every length against what is left.
#[derive(Clone, Debug)]
pub struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    /// A reader at the start of `buf`.
    pub fn new(buf: &'a [u8]) -> Reader<'a> {
        Reader { buf, at: 0 }
    }

    /// Bytes read so far.
    pub fn position(&self) -> usize {
        self.at
    }

    /// Bytes left.
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.at
    }

    fn fail<T>(&self, what: &'static str) -> Result<T, DecodeError> {
        Err(DecodeError { at: self.at, what })
    }

    /// The next `n` bytes.
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if n > self.remaining() {
            return self.fail("runs past the payload");
        }
        let out = &self.buf[self.at..self.at + n];
        self.at += n;
        Ok(out)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        Ok(self.bytes(N)?.try_into().expect("N bytes"))
    }

    /// A `u8`.
    pub fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.array::<1>()?[0])
    }
    /// A `u16`.
    pub fn u16(&mut self) -> Result<u16, DecodeError> {
        self.array().map(u16::from_le_bytes)
    }
    /// A `u32`.
    pub fn u32(&mut self) -> Result<u32, DecodeError> {
        self.array().map(u32::from_le_bytes)
    }
    /// A `u64`.
    pub fn u64(&mut self) -> Result<u64, DecodeError> {
        self.array().map(u64::from_le_bytes)
    }
    /// An `f32`.
    pub fn f32(&mut self) -> Result<f32, DecodeError> {
        self.array().map(f32::from_le_bytes)
    }
    /// An `f64`.
    pub fn f64(&mut self) -> Result<f64, DecodeError> {
        self.array().map(f64::from_le_bytes)
    }
    /// Skip `n` bytes.
    pub fn skip(&mut self, n: usize) -> Result<(), DecodeError> {
        self.bytes(n).map(drop)
    }

    /// A `str`. Not valid UTF-8 is a failed check.
    pub fn str(&mut self) -> Result<&'a str, DecodeError> {
        let n = self.u16()? as usize;
        let at = self.at;
        let b = self.bytes(n)?;
        core::str::from_utf8(b).map_err(|_| DecodeError {
            at,
            what: "a str that is not UTF-8",
        })
    }

    /// An `lv` value.
    pub fn lv(&mut self) -> Result<Lv, DecodeError> {
        self.lv_at(0)
    }

    fn lv_at(&mut self, depth: usize) -> Result<Lv, DecodeError> {
        let tag = u64::from(self.u8()?);
        Ok(match tag {
            lv_tag::NIL => Lv::Nil,
            lv_tag::FALSE => Lv::Bool(false),
            lv_tag::TRUE => Lv::Bool(true),
            lv_tag::NUMBER => Lv::Number(self.f64()?),
            lv_tag::STRING => {
                let n = self.u32()? as usize;
                Lv::String(self.bytes(n)?.to_vec())
            }
            lv_tag::TABLE => {
                if depth >= MAX_DEPTH {
                    return self.fail("tables nest more than 16 deep");
                }
                let n = self.u32()?;
                if n > MAX_PAIRS {
                    return self.fail("a table of more than 65,536 pairs");
                }
                // Each pair takes at least two bytes, so a count the payload
                // cannot hold fails before anything is reserved for it.
                if n as usize > self.remaining() / 2 {
                    return self.fail("runs past the payload");
                }
                let mut pairs = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    let k = self.lv_at(depth + 1)?;
                    match k {
                        Lv::Nil => return self.fail("a nil key"),
                        Lv::Number(x) if x.is_nan() => return self.fail("a NaN key"),
                        _ => {}
                    }
                    let v = self.lv_at(depth + 1)?;
                    pairs.push((k, v));
                }
                Lv::Table(pairs)
            }
            lv_tag::OTHER => Lv::Other(self.str()?.to_owned()),
            _ => return self.fail("an unknown lv tag"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip() {
        let v = Lv::Table(vec![
            (Lv::Number(1.0), Lv::String(b"\xff raw".to_vec())),
            (
                Lv::String(b"k".to_vec()),
                Lv::Table(vec![(Lv::Bool(true), Lv::Nil)]),
            ),
            (Lv::Bool(false), Lv::Other("function".into())),
        ]);
        let mut w = Writer::new();
        w.lv(&v).str("tail");
        let b = w.finish();
        let mut r = Reader::new(&b);
        assert_eq!(r.lv(), Ok(v));
        assert_eq!(r.str(), Ok("tail"));
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn strings_are_made_valid_and_cut_at_a_boundary() {
        let mut w = Writer::new();
        w.str_cut(b"a\xffb", 100).str_cut("ééé".as_bytes(), 5);
        let b = w.finish();
        let mut r = Reader::new(&b);
        assert_eq!(r.str(), Ok("a\u{fffd}b"));
        assert_eq!(r.str(), Ok("éé"));
        let long = vec![b'x'; 70_000];
        let mut w = Writer::new();
        w.str_cut(&long, usize::MAX);
        assert_eq!(w.len(), 2 + MAX_STR);
    }

    #[test]
    fn the_decoder_rejects_what_breaks_a_limit() {
        let deep = (0..=MAX_DEPTH).fold(vec![lv_tag::NIL as u8], |inner, _| {
            let mut t = vec![lv_tag::TABLE as u8, 1, 0, 0, 0, lv_tag::TRUE as u8];
            t.extend(inner);
            t
        });
        assert!(Reader::new(&deep).lv().is_err(), "17 deep");
        assert!(
            Reader::new(&[5, 1, 0, 0, 0, 0, 2]).lv().is_err(),
            "a nil key"
        );
        let mut nan = vec![5, 1, 0, 0, 0, 3];
        nan.extend(f64::NAN.to_le_bytes());
        nan.push(2);
        assert!(Reader::new(&nan).lv().is_err(), "a NaN key");
        assert!(
            Reader::new(&[5, 0xff, 0xff, 0xff, 0xff]).lv().is_err(),
            "a huge count"
        );
        assert!(
            Reader::new(&[4, 9, 0, 0, 0, 1]).lv().is_err(),
            "past the payload"
        );
        assert!(Reader::new(&[7]).lv().is_err(), "an unknown tag");
        assert!(Reader::new(&[2, 0, 0xff, 0xfe]).str().is_err(), "not UTF-8");
    }

    #[test]
    fn the_encoder_writes_nil_past_the_depth_limit() {
        let mut v = Lv::Bool(true);
        for _ in 0..20 {
            v = Lv::Table(vec![(Lv::Number(1.0), v)]);
        }
        let mut w = Writer::new();
        w.lv(&v);
        assert!(Reader::new(&w.finish()).lv().is_ok());
    }
}
