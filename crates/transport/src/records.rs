//! The control records, `LOG` and the command header, encoded and decoded by
//! their layouts (spec/records.md, spec/commands.md). Other kinds are added by
//! the steps that produce them.

use crate::codec::{DecodeError, Lv, Reader, Writer};
use crate::layout::command;

/// `FRAME`: stamps for the records that follow.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    /// Frame sequence number.
    pub frame: u64,
    /// Model time, seconds.
    pub model_time: f64,
    /// Real time, seconds.
    pub real_time: f64,
    /// Active campaign, 0 for none.
    pub campaign: u64,
    /// The number of the record after this one.
    pub number: u64,
    /// Mission generation.
    pub mission_gen: u32,
    /// A `lifecycle` code.
    pub lifecycle: u32,
    /// 1 when the stamps are of an earlier frame.
    pub replay: u32,
}

impl Frame {
    /// The payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u64(self.frame)
            .f64(self.model_time)
            .f64(self.real_time)
            .u64(self.campaign)
            .u64(self.number)
            .u32(self.mission_gen)
            .u32(self.lifecycle)
            .u32(self.replay)
            .pad(4);
        w.finish()
    }

    /// Decode a payload.
    pub fn decode(b: &[u8]) -> Result<Frame, DecodeError> {
        let mut r = Reader::new(b);
        let f = Frame {
            frame: r.u64()?,
            model_time: r.f64()?,
            real_time: r.f64()?,
            campaign: r.u64()?,
            number: r.u64()?,
            mission_gen: r.u32()?,
            lifecycle: r.u32()?,
            replay: r.u32()?,
        };
        r.skip(4)?;
        Ok(f)
    }
}

/// `HELLO`: the agent attached, or resynced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hello {
    /// The agent's boot.
    pub agent_boot: u64,
    /// The sidecar boot the agent attached to.
    pub sidecar_boot: u64,
    /// The install's build.
    pub build: u64,
    /// Active campaign.
    pub campaign: u64,
    /// Command epoch.
    pub epoch: u64,
    /// Highest sequence number handled in `epoch`.
    pub last_applied_seq: u64,
    /// The inbound read position.
    pub in_head: u64,
    /// Interface version.
    pub interface: u32,
    /// `agent_flags` bits.
    pub flags: u32,
    /// The DCS version string.
    pub dcs_version: String,
}

impl Hello {
    /// The payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u64(self.agent_boot)
            .u64(self.sidecar_boot)
            .u64(self.build)
            .u64(self.campaign)
            .u64(self.epoch)
            .u64(self.last_applied_seq)
            .u64(self.in_head)
            .u32(self.interface)
            .u32(self.flags)
            .str(&self.dcs_version);
        w.finish()
    }

    /// Decode a payload.
    pub fn decode(b: &[u8]) -> Result<Hello, DecodeError> {
        let mut r = Reader::new(b);
        Ok(Hello {
            agent_boot: r.u64()?,
            sidecar_boot: r.u64()?,
            build: r.u64()?,
            campaign: r.u64()?,
            epoch: r.u64()?,
            last_applied_seq: r.u64()?,
            in_head: r.u64()?,
            interface: r.u32()?,
            flags: r.u32()?,
            dcs_version: r.str()?.to_owned(),
        })
    }
}

/// `LIFECYCLE`: the lifecycle state changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lifecycle {
    /// A `lifecycle` code.
    pub state: u32,
    /// Mission generation.
    pub mission_gen: u32,
    /// The theater.
    pub theater: String,
    /// The mission.
    pub mission: String,
}

impl Lifecycle {
    /// The payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(self.state)
            .u32(self.mission_gen)
            .str(&self.theater)
            .str(&self.mission);
        w.finish()
    }

    /// Decode a payload.
    pub fn decode(b: &[u8]) -> Result<Lifecycle, DecodeError> {
        let mut r = Reader::new(b);
        Ok(Lifecycle {
            state: r.u32()?,
            mission_gen: r.u32()?,
            theater: r.str()?.to_owned(),
            mission: r.str()?.to_owned(),
        })
    }
}

/// `RESULT`: a command was handled.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandResult {
    /// The command's sequence number.
    pub seq: u64,
    /// The command's epoch.
    pub epoch: u64,
    /// The command's campaign.
    pub campaign: u64,
    /// The command's entity.
    pub entity: u64,
    /// The command's revision.
    pub rev: u64,
    /// The DCS object, 0 for none.
    pub object: u64,
    /// Time around the handler, µs.
    pub cost_us: u32,
    /// The opcode.
    pub opcode: u16,
    /// A `result` code.
    pub code: u16,
    /// A message, empty for none.
    pub message: String,
    /// The handler's value.
    pub value: Lv,
}

impl CommandResult {
    /// The payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u64(self.seq)
            .u64(self.epoch)
            .u64(self.campaign)
            .u64(self.entity)
            .u64(self.rev)
            .u64(self.object)
            .u32(self.cost_us)
            .u16(self.opcode)
            .u16(self.code)
            .str(&self.message)
            .lv(&self.value);
        w.finish()
    }

    /// Decode a payload.
    pub fn decode(b: &[u8]) -> Result<CommandResult, DecodeError> {
        let mut r = Reader::new(b);
        Ok(CommandResult {
            seq: r.u64()?,
            epoch: r.u64()?,
            campaign: r.u64()?,
            entity: r.u64()?,
            rev: r.u64()?,
            object: r.u64()?,
            cost_us: r.u32()?,
            opcode: r.u16()?,
            code: r.u16()?,
            message: r.str()?.to_owned(),
            value: r.lv()?,
        })
    }
}

/// `GAP`: records were dropped.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Gap {
    /// A `gap_class` code.
    pub class: u32,
    /// Records dropped.
    pub count: u32,
    /// Real time of the first dropped record.
    pub from_real: f64,
    /// Real time of the last.
    pub to_real: f64,
    /// Model time of the first.
    pub from_model: f64,
    /// Model time of the last.
    pub to_model: f64,
    /// Mission generation of the first.
    pub from_gen: u32,
    /// Mission generation of the last.
    pub to_gen: u32,
}

impl Gap {
    /// The payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(self.class)
            .u32(self.count)
            .f64(self.from_real)
            .f64(self.to_real)
            .f64(self.from_model)
            .f64(self.to_model)
            .u32(self.from_gen)
            .u32(self.to_gen);
        w.finish()
    }

    /// Decode a payload.
    pub fn decode(b: &[u8]) -> Result<Gap, DecodeError> {
        let mut r = Reader::new(b);
        Ok(Gap {
            class: r.u32()?,
            count: r.u32()?,
            from_real: r.f64()?,
            to_real: r.f64()?,
            from_model: r.f64()?,
            to_model: r.f64()?,
            from_gen: r.u32()?,
            to_gen: r.u32()?,
        })
    }
}

/// `LOG`: a log line.
#[derive(Clone, Debug, PartialEq)]
pub struct Log {
    /// 1 error, 2 warn, 3 info, 4 debug.
    pub level: u32,
    /// The writer.
    pub target: String,
    /// The message.
    pub msg: String,
    /// The `data` table, or nil.
    pub data: Lv,
}

impl Log {
    /// The payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(self.level)
            .pad(4)
            .str(&self.target)
            .str(&self.msg)
            .lv(&self.data);
        w.finish()
    }

    /// Decode a payload.
    pub fn decode(b: &[u8]) -> Result<Log, DecodeError> {
        let mut r = Reader::new(b);
        let level = r.u32()?;
        r.skip(4)?;
        Ok(Log {
            level,
            target: r.str()?.to_owned(),
            msg: r.str()?.to_owned(),
            data: r.lv()?,
        })
    }
}

/// `EVENT`: a DCS world event, its ID and the allow-listed fields the shim
/// read from it (spec/records.md, "Layout: EVENT").
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Event {
    /// The DCS event ID.
    pub id: u32,
    /// Each an `event_field` code and its value, in the order written. A
    /// decoder keeps codes it does not know: later steps add codes.
    pub fields: Vec<(u16, Lv)>,
}

impl Event {
    /// The payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(self.id).u32(self.fields.len() as u32);
        for (code, value) in &self.fields {
            w.u16(*code).lv(value);
        }
        w.finish()
    }

    /// Decode a payload.
    pub fn decode(b: &[u8]) -> Result<Event, DecodeError> {
        let mut r = Reader::new(b);
        let id = r.u32()?;
        let n = r.u32()?;
        // A field takes at least three bytes, a code and a tag, so a count
        // the payload cannot hold reserves nothing.
        let mut fields = Vec::with_capacity((n as usize).min(r.remaining() / 3));
        for _ in 0..n {
            let code = r.u16()?;
            fields.push((code, r.lv()?));
        }
        Ok(Event { id, fields })
    }

    /// The first value with `code`.
    pub fn get(&self, code: u64) -> Option<&Lv> {
        self.fields
            .iter()
            .find(|(c, _)| u64::from(*c) == code)
            .map(|(_, v)| v)
    }

    /// The value with `code` as a number, if it is one.
    pub fn number(&self, code: u64) -> Option<f64> {
        match self.get(code) {
            Some(Lv::Number(n)) => Some(*n),
            _ => None,
        }
    }

    /// The value with `code` as a DCS object ID: a number that is a whole,
    /// non-negative value below 2^53. Anything else is no ID.
    pub fn object(&self, code: u64) -> Option<u64> {
        let n = self.number(code)?;
        (n.is_finite() && n >= 0.0 && n.fract() == 0.0 && n < 9_007_199_254_740_992.0)
            .then_some(n as u64)
    }

    /// The value with `code` as text, if it is a string. Bytes that are not
    /// UTF-8 are replaced.
    pub fn text(&self, code: u64) -> Option<String> {
        match self.get(code) {
            Some(Lv::String(s)) => Some(String::from_utf8_lossy(s).into_owned()),
            _ => None,
        }
    }

    /// The record written in place of one over the payload limit, by the
    /// `EVENT` row of spec/records.md, "Oversized records": every string cut
    /// to 1,024 bytes, at a character boundary where it is UTF-8, every
    /// table or other non-scalar value nil, numbers and booleans kept.
    pub fn cut(&self) -> Event {
        let fields = self
            .fields
            .iter()
            .map(|(c, v)| {
                let v = match v {
                    Lv::String(s) => Lv::String(cut_utf8(s, 1024).to_vec()),
                    Lv::Table(_) | Lv::Other(_) => Lv::Nil,
                    v => v.clone(),
                };
                (*c, v)
            })
            .collect();
        Event {
            id: self.id,
            fields,
        }
    }
}

/// `bytes` cut to at most `max` bytes, at a UTF-8 character boundary when
/// the bytes are UTF-8 up to the cut.
fn cut_utf8(bytes: &[u8], max: usize) -> &[u8] {
    if bytes.len() <= max {
        return bytes;
    }
    let mut end = max;
    // Step back over continuation bytes, at most three.
    while end > max.saturating_sub(3) && bytes[end] & 0xC0 == 0x80 {
        end -= 1;
    }
    &bytes[..end]
}

/// The command header at the start of every inbound payload.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommandHeader {
    /// Sequence number, 0 for a control command.
    pub seq: u64,
    /// Command epoch, 0 for a control command.
    pub epoch: u64,
    /// The campaign it was written for.
    pub campaign: u64,
    /// The entity it acts on.
    pub entity: u64,
    /// The revision it carries.
    pub rev: u64,
    /// The mission generation it was written for.
    pub mission_gen: u32,
}

impl CommandHeader {
    /// Append the header to `w`.
    pub fn write(&self, w: &mut Writer) {
        w.u64(self.seq)
            .u64(self.epoch)
            .u64(self.campaign)
            .u64(self.entity)
            .u64(self.rev)
            .u32(self.mission_gen)
            .pad(4);
    }

    /// Read the header from the start of a payload; the reader is left at the
    /// opcode's own layout.
    pub fn read(r: &mut Reader<'_>) -> Result<CommandHeader, DecodeError> {
        let start = r.position();
        let h = CommandHeader {
            seq: r.u64()?,
            epoch: r.u64()?,
            campaign: r.u64()?,
            entity: r.u64()?,
            rev: r.u64()?,
            mission_gen: r.u32()?,
        };
        r.skip(4)?;
        debug_assert_eq!(r.position() - start, command::HEADER);
        Ok(h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::golden;

    fn fixed(name: &str) -> usize {
        golden::layout(name).expect(name).fixed
    }

    #[test]
    fn fixed_parts_have_the_golden_sizes() {
        assert_eq!(Frame::default().encode().len(), fixed("FRAME"));
        assert_eq!(Hello::default().encode().len(), fixed("HELLO") + 2);
        assert_eq!(Lifecycle::default().encode().len(), fixed("LIFECYCLE") + 4);
        let r = CommandResult {
            seq: 0,
            epoch: 0,
            campaign: 0,
            entity: 0,
            rev: 0,
            object: 0,
            cost_us: 0,
            opcode: 0,
            code: 0,
            message: String::new(),
            value: Lv::Nil,
        };
        assert_eq!(r.encode().len(), fixed("RESULT") + 3);
        assert_eq!(Gap::default().encode().len(), fixed("GAP"));
        let log = Log {
            level: 3,
            target: String::new(),
            msg: String::new(),
            data: Lv::Nil,
        };
        assert_eq!(log.encode().len(), fixed("LOG") + 5);
        assert_eq!(Event::default().encode().len(), fixed("EVENT"));
        let mut w = Writer::new();
        CommandHeader::default().write(&mut w);
        assert_eq!(w.len(), fixed("command"));
    }

    /// Every fixed field of `layout` that is not padding: the bytes at its
    /// golden offset are the value given for it. A field given no value, or a
    /// value for no field, fails.
    fn at_golden_offsets(layout: &str, bytes: &[u8], values: &[(&str, u64)]) {
        let l = golden::layout(layout).expect(layout);
        let fields: Vec<_> = l.fields.iter().filter(|f| f.element().0 != "pad").collect();
        assert_eq!(
            fields.len(),
            values.len(),
            "{layout}: a value for each field"
        );
        for f in fields {
            let (_, v) = values
                .iter()
                .find(|(n, _)| *n == f.name)
                .unwrap_or_else(|| panic!("{layout}.{} has no value", f.name));
            let w = f.size();
            assert_eq!(
                bytes[f.offset..f.offset + w],
                v.to_le_bytes()[..w],
                "{layout}.{}",
                f.name
            );
        }
    }

    #[test]
    fn every_field_sits_at_its_golden_offset_and_decodes() {
        let f = Frame {
            frame: 1,
            model_time: f64::from_bits(2),
            real_time: f64::from_bits(3),
            campaign: 4,
            number: 5,
            mission_gen: 6,
            lifecycle: 7,
            replay: 8,
        };
        let names = [
            ("frame", 1),
            ("model_time", 2),
            ("real_time", 3),
            ("campaign", 4),
            ("number", 5),
            ("mission_gen", 6),
            ("lifecycle", 7),
            ("replay", 8),
        ];
        at_golden_offsets("FRAME", &f.encode(), &names);
        assert_eq!(
            Frame::decode(&f.encode()).map(|d| d.encode()),
            Ok(f.encode())
        );

        let h = Hello {
            agent_boot: 1,
            sidecar_boot: 2,
            build: 3,
            campaign: 4,
            epoch: 5,
            last_applied_seq: 6,
            in_head: 7,
            interface: 8,
            flags: 9,
            dcs_version: String::new(),
        };
        let names = [
            ("agent_boot", 1),
            ("sidecar_boot", 2),
            ("build", 3),
            ("campaign", 4),
            ("epoch", 5),
            ("last_applied_seq", 6),
            ("in_head", 7),
            ("interface", 8),
            ("flags", 9),
        ];
        at_golden_offsets("HELLO", &h.encode(), &names);
        assert_eq!(Hello::decode(&h.encode()), Ok(h));

        let l = Lifecycle {
            state: 1,
            mission_gen: 2,
            theater: String::new(),
            mission: String::new(),
        };
        at_golden_offsets(
            "LIFECYCLE",
            &l.encode(),
            &[("state", 1), ("mission_gen", 2)],
        );
        assert_eq!(Lifecycle::decode(&l.encode()), Ok(l));

        let r = CommandResult {
            seq: 1,
            epoch: 2,
            campaign: 3,
            entity: 4,
            rev: 5,
            object: 6,
            cost_us: 7,
            opcode: 8,
            code: 9,
            message: String::new(),
            value: Lv::Nil,
        };
        let names = [
            ("seq", 1),
            ("epoch", 2),
            ("campaign", 3),
            ("entity", 4),
            ("rev", 5),
            ("object", 6),
            ("cost_us", 7),
            ("opcode", 8),
            ("code", 9),
        ];
        at_golden_offsets("RESULT", &r.encode(), &names);
        assert_eq!(CommandResult::decode(&r.encode()), Ok(r));

        let g = Gap {
            class: 1,
            count: 2,
            from_real: f64::from_bits(3),
            to_real: f64::from_bits(4),
            from_model: f64::from_bits(5),
            to_model: f64::from_bits(6),
            from_gen: 7,
            to_gen: 8,
        };
        let names = [
            ("class", 1),
            ("count", 2),
            ("from_real", 3),
            ("to_real", 4),
            ("from_model", 5),
            ("to_model", 6),
            ("from_gen", 7),
            ("to_gen", 8),
        ];
        at_golden_offsets("GAP", &g.encode(), &names);
        assert_eq!(Gap::decode(&g.encode()).map(|d| d.encode()), Ok(g.encode()));

        let log = Log {
            level: 3,
            target: String::new(),
            msg: String::new(),
            data: Lv::Nil,
        };
        at_golden_offsets("LOG", &log.encode(), &[("level", 3)]);

        let c = CommandHeader {
            seq: 1,
            epoch: 2,
            campaign: 3,
            entity: 4,
            rev: 5,
            mission_gen: 6,
        };
        let mut w = Writer::new();
        c.write(&mut w);
        let b = w.finish();
        let names = [
            ("seq", 1),
            ("epoch", 2),
            ("campaign", 3),
            ("entity", 4),
            ("rev", 5),
            ("mission_gen", 6),
        ];
        at_golden_offsets("command", &b, &names);
        assert_eq!(CommandHeader::read(&mut Reader::new(&b)), Ok(c));
    }

    #[test]
    fn variable_parts_follow_in_golden_order() {
        let h = golden::layout("HELLO").expect("HELLO");
        assert_eq!(
            h.variable.iter().map(|v| v.name).collect::<Vec<_>>(),
            ["dcs_version"]
        );
        let l = golden::layout("LIFECYCLE").expect("LIFECYCLE");
        assert_eq!(
            l.variable.iter().map(|v| v.name).collect::<Vec<_>>(),
            ["theater", "mission"]
        );
        let r = golden::layout("RESULT").expect("RESULT");
        assert_eq!(
            r.variable
                .iter()
                .map(|v| (v.ty, v.name))
                .collect::<Vec<_>>(),
            [("str", "message"), ("lv", "value")]
        );
        let g = golden::layout("LOG").expect("LOG");
        assert_eq!(
            g.variable
                .iter()
                .map(|v| (v.ty, v.name))
                .collect::<Vec<_>>(),
            [("str", "target"), ("str", "msg"), ("lv", "data")]
        );
        let lc = Lifecycle {
            state: 0,
            mission_gen: 0,
            theater: "t".into(),
            mission: "mm".into(),
        };
        assert_eq!(lc.encode()[8..], [1, 0, b't', 2, 0, b'm', b'm']);
    }

    #[test]
    fn records_round_trip() {
        let h = Hello {
            dcs_version: "2.9".into(),
            epoch: 4,
            ..Hello::default()
        };
        assert_eq!(Hello::decode(&h.encode()), Ok(h));
        let l = Lifecycle {
            state: 2,
            mission_gen: 3,
            theater: "Caucasus".into(),
            mission: "m".into(),
        };
        assert_eq!(Lifecycle::decode(&l.encode()), Ok(l));
        let log = Log {
            level: 1,
            target: "agent".into(),
            msg: "x".into(),
            data: Lv::Number(1.0),
        };
        assert_eq!(Log::decode(&log.encode()), Ok(log));
        assert!(Frame::decode(&[0; 10]).is_err());
    }

    #[test]
    fn an_event_round_trips_and_reads_its_fields() {
        let e = Event {
            id: 28,
            fields: vec![
                (1, Lv::Number(12.5)),
                (2, Lv::Number(16_777_472.0)),
                (5, Lv::String(b"AIM_120C".to_vec())),
                (99, Lv::Bool(true)),
            ],
        };
        let d = Event::decode(&e.encode()).expect("decodes");
        assert_eq!(d, e);
        assert_eq!(d.number(1), Some(12.5));
        assert_eq!(d.object(2), Some(16_777_472));
        assert_eq!(d.text(5).as_deref(), Some("AIM_120C"));
        assert_eq!(d.get(99), Some(&Lv::Bool(true)));
        assert_eq!(d.get(3), None);
    }

    #[test]
    fn an_object_id_is_a_whole_non_negative_number() {
        for bad in [-1.0, 1.5, f64::NAN, f64::INFINITY, 1e300] {
            let e = Event {
                id: 1,
                fields: vec![(2, Lv::Number(bad))],
            };
            assert_eq!(e.object(2), None, "{bad}");
        }
        let e = Event {
            id: 1,
            fields: vec![(2, Lv::String(b"7".to_vec()))],
        };
        assert_eq!(e.object(2), None);
    }

    #[test]
    fn an_event_count_past_the_payload_fails_without_reserving() {
        let mut w = Writer::new();
        w.u32(1).u32(u32::MAX);
        assert!(Event::decode(&w.finish()).is_err());
    }

    #[test]
    fn a_cut_event_has_short_strings_and_no_tables() {
        // "a" then two-byte characters: byte 1,024 is a continuation byte,
        // so the cut steps back one byte to 1,023.
        let text = format!("a{}", "\u{e9}".repeat(1000));
        // Bytes that are not UTF-8: the cut stops stepping back after three.
        let raw = vec![0x80u8; 2000];
        let e = Event {
            id: 25,
            fields: vec![
                (8, Lv::String(text.into_bytes())),
                (15, Lv::String(raw)),
                (9, Lv::Number(4.0)),
                (10, Lv::Table(vec![(Lv::Number(1.0), Lv::Bool(true))])),
                (11, Lv::Bool(true)),
            ],
        };
        let c = e.cut();
        let Some(Lv::String(cut)) = c.get(8) else {
            panic!("no text")
        };
        assert_eq!(cut.len(), 1023);
        assert!(std::str::from_utf8(cut).is_ok());
        let Some(Lv::String(cut_raw)) = c.get(15) else {
            panic!("no raw")
        };
        assert_eq!(cut_raw.len(), 1021);
        assert_eq!(c.number(9), Some(4.0));
        assert_eq!(c.get(10), Some(&Lv::Nil));
        assert_eq!(c.get(11), Some(&Lv::Bool(true)));
    }
}
