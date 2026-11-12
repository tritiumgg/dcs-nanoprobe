//! Decoding of record and command payloads from their golden layouts.
//!
//! The fixed fields of any kind are read from the layout table, so a kind needs
//! no code of its own. Variable parts (`str`, `lv`, `tlv[n]`, `T[n]`) are
//! decoded where they parse. What does not parse is shown as hex.

use nanoprobe_transport::codec::{DecodeError, Lv, Reader};
use nanoprobe_transport::golden::{self, Layout};

use crate::util::{clip, flag_names, hex_bytes, named};

/// Bytes of an undecoded rest shown as hex.
const HEX_BYTES: usize = 48;
/// Elements of an array shown.
const ARRAY_SHOWN: usize = 8;
/// Characters of one decoded string or `lv` shown.
const VALUE_CHARS: usize = 200;

/// Decode the payload of an outbound record of the layout `name`.
pub fn record(name: Option<&str>, payload: &[u8]) -> String {
    let mut d = Decoder::new(payload);
    match name.and_then(|n| golden::layout(n).map(|l| (n, l))) {
        Some((n, l)) => {
            d.layout(n, &l);
        }
        None => d.parts.push("layout unknown".into()),
    }
    d.finish()
}

/// Decode an inbound payload: the 48-byte command header, then the opcode's
/// own layout.
pub fn command(opcode: Option<&str>, payload: &[u8]) -> String {
    let mut d = Decoder::new(payload);
    if let Some(header) = golden::layout("command")
        && d.layout("command", &header)
    {
        match opcode.and_then(|n| golden::layout(n).map(|l| (n, l))) {
            Some((n, l)) => {
                d.layout(n, &l);
            }
            None if opcode.is_none() => d.parts.push("opcode unknown".into()),
            None => {}
        }
    }
    d.finish()
}

enum Scalar {
    U(u64),
    F32(f32),
    F64(f64),
}

struct Decoder<'a> {
    data: &'a [u8],
    r: Reader<'a>,
    parts: Vec<String>,
    values: Vec<(&'static str, u64)>,
    mark: usize,
    error: Option<DecodeError>,
}

impl<'a> Decoder<'a> {
    fn new(data: &'a [u8]) -> Self {
        Decoder {
            data,
            r: Reader::new(data),
            parts: Vec::new(),
            values: Vec::new(),
            mark: 0,
            error: None,
        }
    }

    /// Decode one layout. False when a field failed; nothing more is decoded.
    fn layout(&mut self, ctx: &str, l: &Layout<'static>) -> bool {
        match self.try_layout(ctx, l) {
            Ok(()) => true,
            Err(e) => {
                self.error = Some(e);
                false
            }
        }
    }

    fn try_layout(&mut self, ctx: &str, l: &Layout<'static>) -> Result<(), DecodeError> {
        for f in &l.fields {
            self.mark = self.r.position();
            let (ty, n) = f.element();
            if ty == "pad" {
                self.r.skip(n)?;
            } else if n == 1 {
                let v = self.scalar(ty)?;
                if let Scalar::U(u) = v {
                    self.values.push((f.name, u));
                }
                self.parts
                    .push(format!("{}={}", f.name, show(ctx, f.name, &v)));
            } else {
                let mut shown = Vec::new();
                for i in 0..n {
                    let v = self.scalar(ty)?;
                    if i < ARRAY_SHOWN {
                        shown.push(show(ctx, f.name, &v));
                    }
                }
                self.parts.push(format!("{}={}", f.name, list(&shown, n)));
            }
        }
        for v in &l.variable {
            self.mark = self.r.position();
            let text = self.variable(ctx, v.ty)?;
            self.parts.push(format!("{}={text}", v.name));
        }
        Ok(())
    }

    fn scalar(&mut self, ty: &str) -> Result<Scalar, DecodeError> {
        Ok(match ty {
            "u8" => Scalar::U(self.r.u8()?.into()),
            "u16" => Scalar::U(self.r.u16()?.into()),
            "u32" => Scalar::U(self.r.u32()?.into()),
            "u64" => Scalar::U(self.r.u64()?),
            "f32" => Scalar::F32(self.r.f32()?),
            "f64" => Scalar::F64(self.r.f64()?),
            _ => {
                return Err(DecodeError {
                    at: self.r.position(),
                    what: "a field type the layout reader does not know",
                });
            }
        })
    }

    fn variable(&mut self, ctx: &str, ty: &str) -> Result<String, DecodeError> {
        match ty {
            "str" => Ok(clip(format!("{:?}", self.r.str()?), VALUE_CHARS)),
            "lv" => Ok(lv_text(&self.r.lv()?)),
            _ => {
                let bad = |at| DecodeError {
                    at,
                    what: "a variable part the layout reader does not know",
                };
                let at = self.r.position();
                let (elem, count_name) = ty
                    .split_once('[')
                    .and_then(|(e, c)| Some((e, c.strip_suffix(']')?)))
                    .ok_or_else(|| bad(at))?;
                let count = self
                    .values
                    .iter()
                    .rev()
                    .find(|(n, _)| *n == count_name)
                    .map(|&(_, v)| v)
                    .ok_or_else(|| bad(at))?;
                if elem == "tlv" {
                    self.tlv(ctx, count)
                } else {
                    self.array(elem, count)
                }
            }
        }
    }

    fn array(&mut self, elem: &str, count: u64) -> Result<String, DecodeError> {
        let at = self.r.position();
        let width = golden::width(elem).ok_or(DecodeError {
            at,
            what: "an array of an unknown type",
        })?;
        let fits = usize::try_from(count)
            .ok()
            .filter(|c| {
                c.checked_mul(width)
                    .is_some_and(|b| b <= self.r.remaining())
            })
            .ok_or(DecodeError {
                at,
                what: "an array count that runs past the payload",
            })?;
        let mut shown = Vec::new();
        for i in 0..fits {
            let v = self.scalar(elem)?;
            if i < ARRAY_SHOWN {
                shown.push(show("", "", &v));
            }
        }
        Ok(list(&shown, fits))
    }

    fn tlv(&mut self, ctx: &str, count: u64) -> Result<String, DecodeError> {
        let mut items = Vec::new();
        for i in 0..count {
            let code = u64::from(self.r.u16()?);
            let value = lv_text(&self.r.lv()?);
            if i < ARRAY_SHOWN as u64 {
                let name = golden::code_name("event_field", code)
                    .filter(|_| ctx == "EVENT")
                    .map_or_else(|| code.to_string(), str::to_string);
                items.push(format!("{name}:{value}"));
            }
        }
        Ok(list(&items, usize::try_from(count).unwrap_or(usize::MAX)))
    }

    fn finish(mut self) -> String {
        if let Some(e) = &self.error {
            self.parts.push(format!("error=\"{e}\""));
            let rest = &self.data[self.mark.min(self.data.len())..];
            self.parts.push(format!(
                "rest[{}]={}",
                rest.len(),
                hex_bytes(rest, HEX_BYTES)
            ));
        } else if self.r.remaining() > 0 {
            let rest = &self.data[self.r.position()..];
            self.parts.push(format!(
                "trailing[{}]={}",
                rest.len(),
                hex_bytes(rest, HEX_BYTES)
            ));
        }
        self.parts.join(" ")
    }
}

/// `[a, b, c]`, with the total when more elements exist than are shown.
fn list(shown: &[String], total: usize) -> String {
    if total > shown.len() {
        format!("[{}, ..({total} in all)]", shown.join(", "))
    } else {
        format!("[{}]", shown.join(", "))
    }
}

/// The section of code names a field of a layout takes its values from.
fn code_section(ctx: &str, field: &str) -> Option<&'static str> {
    Some(match (ctx, field) {
        ("FRAME", "lifecycle") | ("LIFECYCLE", "state") => "lifecycle",
        ("HELLO", "flags") => "agent_flags",
        ("RESULT", "code") => "result",
        ("RESULT", "opcode") => "opcode",
        ("GAP", "class") => "gap_class",
        ("OBJECT", "kind") => "object_kind",
        ("OBJECT_GONE", "reason") => "gone_reason",
        ("PLAYER", "event") => "player_event",
        ("MARK_SET" | "DRAWING_SET" | "MENU_SET" | "MENU" | "MESSAGE" | "SOUND", "scope") => {
            "scope"
        }
        _ => return None,
    })
}

fn show(ctx: &str, field: &str, v: &Scalar) -> String {
    match v {
        Scalar::F32(f) => f.to_string(),
        Scalar::F64(f) => f.to_string(),
        Scalar::U(u) => match code_section(ctx, field) {
            Some("agent_flags") => flag_names("agent_flags", *u),
            Some(section) => named(section, *u),
            None if field.contains("boot") || field == "build" => format!("{u:#x}"),
            None => u.to_string(),
        },
    }
}

/// A Lua value as text: `nil`, `true`, `1.5`, `"text"`, `{k=v, ...}`.
pub fn lv_text(v: &Lv) -> String {
    let mut out = String::new();
    lv_into(v, &mut out);
    clip(out, VALUE_CHARS)
}

fn lv_into(v: &Lv, out: &mut String) {
    // The caller clips the whole text, so stop growing once it is past that.
    if out.len() > 4 * VALUE_CHARS {
        return;
    }
    match v {
        Lv::Nil => out.push_str("nil"),
        Lv::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Lv::Number(n) => out.push_str(&n.to_string()),
        Lv::String(s) => out.push_str(&format!("{:?}", String::from_utf8_lossy(s))),
        Lv::Other(name) => out.push_str(&format!("<{name}>")),
        Lv::Table(pairs) => {
            out.push('{');
            for (i, (k, val)) in pairs.iter().enumerate() {
                if i == ARRAY_SHOWN * 2 {
                    out.push_str(&format!(", ..({} pairs)", pairs.len()));
                    break;
                }
                if i > 0 {
                    out.push_str(", ");
                }
                lv_into(k, out);
                out.push('=');
                lv_into(val, out);
            }
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanoprobe_transport::codec::Writer;

    #[test]
    fn fixed_fields_and_strings_decode() {
        let mut w = Writer::new();
        w.u32(3).u32(7).str("Caucasus").str("m1");
        let text = record(Some("LIFECYCLE"), &w.finish());
        assert_eq!(
            text,
            "state=RUNNING(3) mission_gen=7 theater=\"Caucasus\" mission=\"m1\""
        );
    }

    #[test]
    fn arrays_and_lv_decode() {
        let mut w = Writer::new();
        w.u32(2).pad(4).u64(10).u64(11);
        for v in [1.0f64, 2.0, 3.0, 4.0, 5.0, 6.0] {
            w.f64(v);
        }
        let text = record(Some("POSITIONS"), &w.finish());
        assert!(text.contains("n=2"), "{text}");
        assert!(text.contains("object=[10, 11]"), "{text}");
        assert!(text.contains("z=[5, 6]"), "{text}");

        let mut w = Writer::new();
        w.u64(5).lv(&Lv::Table(vec![(
            Lv::String(b"k".to_vec()),
            Lv::Number(2.0),
        )]));
        assert_eq!(record(Some("AMMO"), &w.finish()), "object=5 ammo={\"k\"=2}");
    }

    #[test]
    fn a_short_or_wrong_payload_falls_back_to_hex() {
        let text = record(Some("GAP"), &[1, 0, 0, 0, 2, 0]);
        assert!(text.contains("class=EVENTS(1)"), "{text}");
        assert!(text.contains("error="), "{text}");
        assert!(text.contains("rest[2]=02 00"), "{text}");
        let mut w = Writer::new();
        w.u32(1_000_000).pad(4);
        let text = record(Some("POSITIONS"), &w.finish());
        assert!(text.contains("runs past the payload"), "{text}");
        assert!(record(None, &[1, 2]).contains("layout unknown"));
    }

    #[test]
    fn a_command_has_its_header_and_opcode_layout() {
        let mut w = Writer::new();
        w.u64(0).u64(0).u64(9).u64(0).u64(0).u32(0).pad(4).u64(42);
        let text = command(Some("CAMPAIGN"), &w.finish());
        assert!(text.contains("campaign=9"), "{text}");
        assert!(text.ends_with("campaign=42"), "{text}");
    }

    #[test]
    fn trailing_bytes_show_as_hex() {
        let mut w = Writer::new();
        w.u64(1).u32(0).pad(4);
        w.u8(0xaa);
        let text = record(Some("OBJECT_GONE"), &w.finish());
        assert!(text.ends_with("trailing[1]=aa"), "{text}");
    }
}
