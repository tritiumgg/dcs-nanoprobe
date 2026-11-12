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
}
