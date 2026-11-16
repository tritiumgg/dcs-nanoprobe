//! The record payloads the agent writes, the command header, and the payloads
//! of the `RATES`, `SUBSCRIBE` and `BIND` commands, each encoded and decoded
//! by its layout (spec/records.md, spec/commands.md). The kinds are the
//! control records, `LOG`, `EVENT`, the object table (`OBJECT`,
//! `OBJECT_GONE`, `ENUM_BEGIN`, `ENUM_END`) and the samples (`POSITIONS`,
//! `VITALS`, `ATTITUDE`, `WAREHOUSE`, `AIRBASE`). A decoder never
//! panics, and a count the payload cannot hold is an error that allocates
//! nothing.

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

/// `OBJECT`: a row of the agent's object table (spec/records.md, "Layout:
/// OBJECT").
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Object {
    /// DCS object ID.
    pub object: u64,
    /// DCS ID of the unit's group, 0 for a static.
    pub group: u64,
    /// The `EntityId` the binding holds for the object, 0 for none.
    pub entity: u64,
    /// The `EntityId` the binding holds for the group, 0 for none.
    pub group_entity: u64,
    /// The enumeration that found it, 0 when an event or a spawn did.
    pub enumeration: u64,
    /// An `object_kind` code.
    pub kind: u32,
    /// DCS's category number.
    pub category: u32,
    /// DCS coalition number.
    pub coalition: u32,
    /// DCS country number.
    pub country: u32,
    /// The life the object started with.
    pub life0: f32,
    /// The object's DCS name.
    pub name: String,
    /// The DCS type name.
    pub type_name: String,
    /// The group's DCS name, empty for a static.
    pub group_name: String,
}

impl Object {
    /// Where `enumeration` sits in the payload, as a little-endian u64
    /// (spec/records.md): a cached payload is patched there.
    pub const ENUMERATION_AT: usize = 0x20;

    /// The payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u64(self.object)
            .u64(self.group)
            .u64(self.entity)
            .u64(self.group_entity)
            .u64(self.enumeration)
            .u32(self.kind)
            .u32(self.category)
            .u32(self.coalition)
            .u32(self.country)
            .f32(self.life0)
            .pad(4)
            .str(&self.name)
            .str(&self.type_name)
            .str(&self.group_name);
        w.finish()
    }

    /// Decode a payload.
    pub fn decode(b: &[u8]) -> Result<Object, DecodeError> {
        let mut r = Reader::new(b);
        let o = Object {
            object: r.u64()?,
            group: r.u64()?,
            entity: r.u64()?,
            group_entity: r.u64()?,
            enumeration: r.u64()?,
            kind: r.u32()?,
            category: r.u32()?,
            coalition: r.u32()?,
            country: r.u32()?,
            life0: r.f32()?,
            name: String::new(),
            type_name: String::new(),
            group_name: String::new(),
        };
        r.skip(4)?;
        Ok(Object {
            name: r.str()?.to_owned(),
            type_name: r.str()?.to_owned(),
            group_name: r.str()?.to_owned(),
            ..o
        })
    }
}

/// `OBJECT_GONE`: a row left the object table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ObjectGone {
    /// DCS object ID.
    pub object: u64,
    /// A `gone_reason` code.
    pub reason: u32,
    /// The `EntityId` the binding held for the object, 0 for none.
    pub entity: u64,
    /// The `EntityId` the binding held for the group, 0 for none.
    pub group_entity: u64,
}

impl ObjectGone {
    /// The payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u64(self.object)
            .u32(self.reason)
            .pad(4)
            .u64(self.entity)
            .u64(self.group_entity);
        w.finish()
    }

    /// Decode a payload.
    pub fn decode(b: &[u8]) -> Result<ObjectGone, DecodeError> {
        let mut r = Reader::new(b);
        let object = r.u64()?;
        let reason = r.u32()?;
        r.skip(4)?;
        Ok(ObjectGone {
            object,
            reason,
            entity: r.u64()?,
            group_entity: r.u64()?,
        })
    }
}

/// `ENUM_BEGIN`: an enumeration started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EnumBegin {
    /// The enumeration's number, never 0.
    pub enumeration: u64,
}

impl EnumBegin {
    /// The payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u64(self.enumeration);
        w.finish()
    }

    /// Decode a payload.
    pub fn decode(b: &[u8]) -> Result<EnumBegin, DecodeError> {
        let mut r = Reader::new(b);
        Ok(EnumBegin {
            enumeration: r.u64()?,
        })
    }
}

/// `ENUM_END`: an enumeration finished.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EnumEnd {
    /// The enumeration that finished.
    pub enumeration: u64,
    /// `OBJECT` records it wrote.
    pub objects: u32,
    /// IDs it wrote in `SEEN` records.
    pub seen: u32,
}

impl EnumEnd {
    /// The payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u64(self.enumeration).u32(self.objects).u32(self.seen);
        w.finish()
    }

    /// Decode a payload.
    pub fn decode(b: &[u8]) -> Result<EnumEnd, DecodeError> {
        let mut r = Reader::new(b);
        Ok(EnumEnd {
            enumeration: r.u64()?,
            objects: r.u32()?,
            seen: r.u32()?,
        })
    }
}

/// `SEEN`: rows an enumeration found whose `OBJECT` is unchanged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    /// The enumeration that found them.
    pub enumeration: u64,
    /// DCS object IDs.
    pub objects: Vec<u64>,
}

impl Seen {
    /// Bytes of the fixed part.
    pub const FIXED: usize = 16;

    /// The most IDs a payload of `max_payload` bytes holds.
    pub const fn capacity(max_payload: usize) -> usize {
        max_payload.saturating_sub(Self::FIXED) / 8
    }

    /// The payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::FIXED + 8 * self.objects.len());
        Self::encode_into(self.enumeration, &self.objects, &mut out);
        out
    }

    /// Write the payload for `objects` into `out`, which is cleared first.
    pub fn encode_into(enumeration: u64, objects: &[u64], out: &mut Vec<u8>) {
        out.clear();
        out.extend_from_slice(&enumeration.to_le_bytes());
        out.extend_from_slice(&(objects.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        for o in objects {
            out.extend_from_slice(&o.to_le_bytes());
        }
    }

    /// Decode a payload.
    pub fn decode(b: &[u8]) -> Result<Seen, DecodeError> {
        let mut r = Reader::new(b);
        let enumeration = r.u64()?;
        let count = r.u32()?;
        r.skip(4)?;
        let objects = u64_column(columns(&mut r, count, 8)?);
        Ok(Seen {
            enumeration,
            objects,
        })
    }
}

/// The row count of a batch, from the length of the shortest column. Encoders
/// write every column to this length.
fn rows(lens: &[usize]) -> usize {
    debug_assert!(lens.windows(2).all(|p| p[0] == p[1]), "unequal columns");
    lens.iter().copied().min().unwrap_or(0)
}

/// Read `n` rows of columns that are `row` bytes wide together. The bytes are
/// taken before anything is allocated, so a count the payload cannot hold is
/// an error that reserves nothing.
fn columns<'a>(r: &mut Reader<'a>, n: u32, row: usize) -> Result<&'a [u8], DecodeError> {
    r.bytes((n as usize).saturating_mul(row))
}

fn u64_column(b: &[u8]) -> Vec<u64> {
    let (chunks, _) = b.as_chunks::<8>();
    chunks.iter().map(|c| u64::from_le_bytes(*c)).collect()
}

fn f64_column(b: &[u8]) -> Vec<f64> {
    let (chunks, _) = b.as_chunks::<8>();
    chunks.iter().map(|c| f64::from_le_bytes(*c)).collect()
}

fn f32_column(b: &[u8]) -> Vec<f32> {
    let (chunks, _) = b.as_chunks::<4>();
    chunks.iter().map(|c| f32::from_le_bytes(*c)).collect()
}

/// `POSITIONS`: a columnar batch of positions. Every column has one entry
/// per object.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Positions {
    /// DCS object IDs.
    pub object: Vec<u64>,
    /// DCS `x`, meters.
    pub x: Vec<f64>,
    /// DCS `y`, meters.
    pub y: Vec<f64>,
    /// DCS `z`, meters.
    pub z: Vec<f64>,
}

impl Positions {
    /// The payload. The columns must have equal lengths; the row count is
    /// the shortest, and a longer column is cut to it.
    pub fn encode(&self) -> Vec<u8> {
        let n = rows(&[self.object.len(), self.x.len(), self.y.len(), self.z.len()]);
        let mut w = Writer::new();
        w.u32(n as u32).pad(4);
        self.object[..n].iter().for_each(|v| _ = w.u64(*v));
        self.x[..n].iter().for_each(|v| _ = w.f64(*v));
        self.y[..n].iter().for_each(|v| _ = w.f64(*v));
        self.z[..n].iter().for_each(|v| _ = w.f64(*v));
        w.finish()
    }

    /// Decode a payload.
    pub fn decode(b: &[u8]) -> Result<Positions, DecodeError> {
        let mut r = Reader::new(b);
        let n = r.u32()?;
        r.skip(4)?;
        let all = columns(&mut r, n, 32)?;
        let (object, rest) = all.split_at(n as usize * 8);
        let (x, rest) = rest.split_at(n as usize * 8);
        let (y, z) = rest.split_at(n as usize * 8);
        Ok(Positions {
            object: u64_column(object),
            x: f64_column(x),
            y: f64_column(y),
            z: f64_column(z),
        })
    }
}

/// `VITALS`: a columnar batch of life and fuel. Every column has one entry
/// per object.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Vitals {
    /// DCS object IDs.
    pub object: Vec<u64>,
    /// `getLife()`.
    pub life: Vec<f32>,
    /// `getFuel()`, NaN for a static.
    pub fuel: Vec<f32>,
}

impl Vitals {
    /// The payload. The columns must have equal lengths; the row count is
    /// the shortest, and a longer column is cut to it.
    pub fn encode(&self) -> Vec<u8> {
        let n = rows(&[self.object.len(), self.life.len(), self.fuel.len()]);
        let mut w = Writer::new();
        w.u32(n as u32).pad(4);
        self.object[..n].iter().for_each(|v| _ = w.u64(*v));
        self.life[..n].iter().for_each(|v| _ = w.f32(*v));
        self.fuel[..n].iter().for_each(|v| _ = w.f32(*v));
        w.finish()
    }

    /// Decode a payload.
    pub fn decode(b: &[u8]) -> Result<Vitals, DecodeError> {
        let mut r = Reader::new(b);
        let n = r.u32()?;
        r.skip(4)?;
        let all = columns(&mut r, n, 16)?;
        let (object, rest) = all.split_at(n as usize * 8);
        let (life, fuel) = rest.split_at(n as usize * 4);
        Ok(Vitals {
            object: u64_column(object),
            life: f32_column(life),
            fuel: f32_column(fuel),
        })
    }
}

/// `ATTITUDE`: a columnar batch of attitudes. Every column has one entry
/// per object.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Attitude {
    /// DCS object IDs.
    pub object: Vec<u64>,
    /// Heading, radians.
    pub heading: Vec<f32>,
    /// Pitch, radians.
    pub pitch: Vec<f32>,
    /// Roll, radians.
    pub roll: Vec<f32>,
}

impl Attitude {
    /// The payload. The columns must have equal lengths; the row count is
    /// the shortest, and a longer column is cut to it.
    pub fn encode(&self) -> Vec<u8> {
        let n = rows(&[
            self.object.len(),
            self.heading.len(),
            self.pitch.len(),
            self.roll.len(),
        ]);
        let mut w = Writer::new();
        w.u32(n as u32).pad(4);
        self.object[..n].iter().for_each(|v| _ = w.u64(*v));
        self.heading[..n].iter().for_each(|v| _ = w.f32(*v));
        self.pitch[..n].iter().for_each(|v| _ = w.f32(*v));
        self.roll[..n].iter().for_each(|v| _ = w.f32(*v));
        w.finish()
    }

    /// Decode a payload.
    pub fn decode(b: &[u8]) -> Result<Attitude, DecodeError> {
        let mut r = Reader::new(b);
        let n = r.u32()?;
        r.skip(4)?;
        let all = columns(&mut r, n, 20)?;
        let (object, rest) = all.split_at(n as usize * 8);
        let (heading, rest) = rest.split_at(n as usize * 4);
        let (pitch, roll) = rest.split_at(n as usize * 4);
        Ok(Attitude {
            object: u64_column(object),
            heading: f32_column(heading),
            pitch: f32_column(pitch),
            roll: f32_column(roll),
        })
    }
}

/// `WAREHOUSE`: the inventory of one airbase.
#[derive(Clone, Debug, PartialEq)]
pub struct Warehouse {
    /// The airbase's DCS name.
    pub airbase: String,
    /// The inventory as plain tables of names and counts.
    pub stock: Lv,
}

impl Warehouse {
    /// The payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.str(&self.airbase).lv(&self.stock);
        w.finish()
    }

    /// Decode a payload.
    pub fn decode(b: &[u8]) -> Result<Warehouse, DecodeError> {
        let mut r = Reader::new(b);
        Ok(Warehouse {
            airbase: r.str()?.to_owned(),
            stock: r.lv()?,
        })
    }
}

/// `AIRBASE`: an airbase and its owner.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Airbase {
    /// The airbase's DCS object ID.
    pub object: u64,
    /// DCS coalition number of the owner.
    pub coalition: u32,
    /// The airbase's DCS name.
    pub airbase: String,
}

impl Airbase {
    /// The payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u64(self.object)
            .u32(self.coalition)
            .pad(4)
            .str(&self.airbase);
        w.finish()
    }

    /// Decode a payload.
    pub fn decode(b: &[u8]) -> Result<Airbase, DecodeError> {
        let mut r = Reader::new(b);
        let object = r.u64()?;
        let coalition = r.u32()?;
        r.skip(4)?;
        Ok(Airbase {
            object,
            coalition,
            airbase: r.str()?.to_owned(),
        })
    }
}

/// `AUTHOR_EVENT`: an event from `emit` or an extension.
#[derive(Clone, Debug, PartialEq)]
pub struct AuthorEvent {
    /// DCS object IDs of the units the event is about.
    pub objects: Vec<u64>,
    /// The event's name.
    pub name: String,
    /// The author's value.
    pub payload: Lv,
}

impl AuthorEvent {
    /// The payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(self.objects.len() as u32).pad(4);
        self.objects.iter().for_each(|v| _ = w.u64(*v));
        w.str(&self.name).lv(&self.payload);
        w.finish()
    }

    /// Decode a payload.
    pub fn decode(b: &[u8]) -> Result<AuthorEvent, DecodeError> {
        let mut r = Reader::new(b);
        let n = r.u32()?;
        r.skip(4)?;
        let objects = u64_column(columns(&mut r, n, 8)?);
        Ok(AuthorEvent {
            objects,
            name: r.str()?.to_owned(),
            payload: r.lv()?,
        })
    }
}

/// `PLAYER`: a player connected, left, changed slot, chatted or was renamed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Player {
    /// A `player_event` code.
    pub event: u32,
    /// The ID DCS gives the connection.
    pub player: u32,
    /// DCS coalition number.
    pub side: u32,
    /// The seat in a multi-crew aircraft, 0 for the first or when DCS gives none.
    pub seat: u32,
    /// The ID DCS gives the client.
    pub client: String,
    /// The player's name.
    pub name: String,
    /// The slot ID.
    pub slot: String,
    /// The chat text, for `CHAT`.
    pub text: String,
}

impl Player {
    /// The payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(self.event)
            .u32(self.player)
            .u32(self.side)
            .u32(self.seat)
            .str(&self.client)
            .str(&self.name)
            .str(&self.slot)
            .str(&self.text);
        w.finish()
    }

    /// Decode a payload.
    pub fn decode(b: &[u8]) -> Result<Player, DecodeError> {
        let mut r = Reader::new(b);
        Ok(Player {
            event: r.u32()?,
            player: r.u32()?,
            side: r.u32()?,
            seat: r.u32()?,
            client: r.str()?.to_owned(),
            name: r.str()?.to_owned(),
            slot: r.str()?.to_owned(),
            text: r.str()?.to_owned(),
        })
    }
}

/// `MENU`: a player selected a menu item.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Menu {
    /// The menu item's `EntityId`.
    pub entity: u64,
    /// What the item was added for, by `scope`: 0, a coalition number or a group `EntityId`.
    pub target: u64,
    /// The item's `scope` code.
    pub scope: u32,
    /// The connection ID of the player who selected it, 0 when DCS does not say.
    pub player: u32,
}

impl Menu {
    /// The payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u64(self.entity)
            .u64(self.target)
            .u32(self.scope)
            .u32(self.player);
        w.finish()
    }

    /// Decode a payload.
    pub fn decode(b: &[u8]) -> Result<Menu, DecodeError> {
        let mut r = Reader::new(b);
        Ok(Menu {
            entity: r.u64()?,
            target: r.u64()?,
            scope: r.u32()?,
            player: r.u32()?,
        })
    }
}

/// `EXTENSION`: one registered extension's run time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtensionStats {
    /// Hook calls since the extension was installed in this mission.
    pub calls: u64,
    /// Time in those runs, microseconds.
    pub total_us: u64,
    /// The longest run, microseconds.
    pub max_us: u64,
    /// The extension's applied revision.
    pub rev: u64,
    /// Events its `on_event` queue dropped.
    pub events_skipped: u64,
    /// 1 when its hooks are stopped after errors.
    pub stopped: u32,
    /// The extension's name.
    pub name: String,
}

impl ExtensionStats {
    /// The payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u64(self.calls)
            .u64(self.total_us)
            .u64(self.max_us)
            .u64(self.rev)
            .u64(self.events_skipped)
            .u32(self.stopped)
            .pad(4)
            .str(&self.name);
        w.finish()
    }

    /// Decode a payload.
    pub fn decode(b: &[u8]) -> Result<ExtensionStats, DecodeError> {
        let mut r = Reader::new(b);
        let calls = r.u64()?;
        let total_us = r.u64()?;
        let max_us = r.u64()?;
        let rev = r.u64()?;
        let events_skipped = r.u64()?;
        let stopped = r.u32()?;
        r.skip(4)?;
        Ok(ExtensionStats {
            calls,
            total_us,
            max_us,
            rev,
            events_skipped,
            stopped,
            name: r.str()?.to_owned(),
        })
    }
}

/// `RATES` command: the bytes after the command header.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rates {
    /// Target period of the position tier, seconds.
    pub position_s: f64,
    /// Target period of the vitals tier.
    pub vitals_s: f64,
    /// Target period of the warehouse and airbase tier.
    pub warehouse_s: f64,
    /// Period of the full enumeration.
    pub enumeration_s: f64,
}

impl Rates {
    /// The bytes after the command header.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.f64(self.position_s)
            .f64(self.vitals_s)
            .pad(8)
            .f64(self.warehouse_s)
            .f64(self.enumeration_s);
        w.finish()
    }

    /// Decode the bytes after the command header.
    pub fn decode(b: &[u8]) -> Result<Rates, DecodeError> {
        let mut r = Reader::new(b);
        let position_s = r.f64()?;
        let vitals_s = r.f64()?;
        r.skip(8)?;
        Ok(Rates {
            position_s,
            vitals_s,
            warehouse_s: r.f64()?,
            enumeration_s: r.f64()?,
        })
    }
}

/// `SUBSCRIBE` command: the bytes after the command header.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Subscribe {
    /// Target period for the listed entities' positions, seconds.
    pub period_s: f64,
    /// 1 to sample attitude with each position, else 0.
    pub attitude: u32,
    /// The `EntityId`s.
    pub entity: Vec<u64>,
}

impl Subscribe {
    /// The bytes after the command header.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.f64(self.period_s)
            .u32(self.attitude)
            .u32(self.entity.len() as u32);
        self.entity.iter().for_each(|v| _ = w.u64(*v));
        w.finish()
    }

    /// Decode the bytes after the command header.
    pub fn decode(b: &[u8]) -> Result<Subscribe, DecodeError> {
        let mut r = Reader::new(b);
        let period_s = r.f64()?;
        let attitude = r.u32()?;
        let n = r.u32()?;
        let entity = u64_column(columns(&mut r, n, 8)?);
        Ok(Subscribe {
            period_s,
            attitude,
            entity,
        })
    }
}

/// `BIND` command: the bytes after the command header.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bind {
    /// An `object_kind` code: what every pair binds.
    pub kind: u32,
    /// The `EntityId`s.
    pub entity: Vec<u64>,
    /// DCS object IDs, or DCS group IDs for a group. One per entity.
    pub object: Vec<u64>,
}

impl Bind {
    /// The bytes after the command header. The columns must have equal
    /// lengths; the pair count is the shorter, and a longer column is cut
    /// to it.
    pub fn encode(&self) -> Vec<u8> {
        let n = rows(&[self.entity.len(), self.object.len()]);
        let mut w = Writer::new();
        w.u32(n as u32).u32(self.kind);
        self.entity[..n].iter().for_each(|v| _ = w.u64(*v));
        self.object[..n].iter().for_each(|v| _ = w.u64(*v));
        w.finish()
    }

    /// Decode the bytes after the command header.
    pub fn decode(b: &[u8]) -> Result<Bind, DecodeError> {
        let mut r = Reader::new(b);
        let n = r.u32()?;
        let kind = r.u32()?;
        let all = columns(&mut r, n, 16)?;
        let (entity, object) = all.split_at(n as usize * 8);
        Ok(Bind {
            kind,
            entity: u64_column(entity),
            object: u64_column(object),
        })
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

    fn stock() -> Lv {
        Lv::Table(vec![(
            Lv::Number(1.0),
            Lv::Table(vec![
                (Lv::String(b"type".to_vec()), Lv::String(b"AIM-9".to_vec())),
                (Lv::String(b"count".to_vec()), Lv::Number(4.0)),
            ]),
        )])
    }

    /// Every strict prefix of `bytes` fails to decode.
    fn truncations_fail<T: std::fmt::Debug>(
        bytes: &[u8],
        decode: impl Fn(&[u8]) -> Result<T, DecodeError>,
    ) {
        for n in 0..bytes.len() {
            assert!(decode(&bytes[..n]).is_err(), "prefix of {n} bytes decoded");
        }
    }

    #[test]
    fn object_records_round_trip() {
        let o = Object {
            object: 1,
            group: 2,
            entity: 3,
            group_entity: 4,
            enumeration: 5,
            kind: 1,
            category: 2,
            coalition: 3,
            country: 4,
            life0: 1.5,
            name: "Pilot-1".into(),
            type_name: "F-16C".into(),
            group_name: "Flight".into(),
        };
        let b = o.encode();
        assert_eq!(Object::decode(&b), Ok(o));
        truncations_fail(&b, Object::decode);
        assert_eq!(Object::default().encode().len(), fixed("OBJECT") + 6);

        let g = ObjectGone {
            object: 1,
            reason: 3,
            entity: 4,
            group_entity: 5,
        };
        let b = g.encode();
        assert_eq!(b.len(), fixed("OBJECT_GONE"));
        assert_eq!(ObjectGone::decode(&b), Ok(g));
        truncations_fail(&b, ObjectGone::decode);

        let e = EnumBegin { enumeration: 7 };
        let b = e.encode();
        assert_eq!(b.len(), fixed("ENUM_BEGIN"));
        assert_eq!(EnumBegin::decode(&b), Ok(e));
        truncations_fail(&b, EnumBegin::decode);

        let e = EnumEnd {
            enumeration: 7,
            objects: 9,
            seen: 1_990,
        };
        let b = e.encode();
        assert_eq!(b.len(), fixed("ENUM_END"));
        assert_eq!(EnumEnd::decode(&b), Ok(e));
        truncations_fail(&b, EnumEnd::decode);

        let s = Seen {
            enumeration: 7,
            objects: vec![3, 1 << 40, u64::MAX],
        };
        let b = s.encode();
        assert_eq!(b.len(), fixed("SEEN") + 24);
        assert_eq!(Seen::decode(&b), Ok(s));
        truncations_fail(&b, Seen::decode);
        assert_eq!(Seen::default().encode().len(), Seen::FIXED);
        assert_eq!(Seen::capacity(Seen::FIXED + 80), 10);
        // A count the payload cannot hold is an error that reserves nothing.
        let mut lie = b.clone();
        lie[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Seen::decode(&lie).is_err());
    }

    #[test]
    fn sample_batches_round_trip() {
        let p = Positions {
            object: vec![1, 2],
            x: vec![1.0, 2.0],
            y: vec![3.0, 4.0],
            z: vec![5.0, 6.0],
        };
        let b = p.encode();
        assert_eq!(b.len(), fixed("POSITIONS") + 2 * 32);
        assert_eq!(Positions::decode(&b), Ok(p));
        truncations_fail(&b, Positions::decode);
        assert_eq!(Positions::default().encode().len(), fixed("POSITIONS"));
        assert_eq!(
            Positions::decode(&Positions::default().encode()),
            Ok(Positions::default())
        );

        let v = Vitals {
            object: vec![1, 2, 3],
            life: vec![1.0, 2.0, 3.0],
            fuel: vec![0.5, f32::MAX, 0.25],
        };
        let b = v.encode();
        assert_eq!(b.len(), fixed("VITALS") + 3 * 16);
        assert_eq!(Vitals::decode(&b), Ok(v));
        truncations_fail(&b, Vitals::decode);
        assert_eq!(Vitals::default().encode().len(), fixed("VITALS"));

        let a = Attitude {
            object: vec![9],
            heading: vec![1.0],
            pitch: vec![2.0],
            roll: vec![3.0],
        };
        let b = a.encode();
        assert_eq!(b.len(), fixed("ATTITUDE") + 20);
        assert_eq!(Attitude::decode(&b), Ok(a));
        truncations_fail(&b, Attitude::decode);
        assert_eq!(Attitude::default().encode().len(), fixed("ATTITUDE"));
    }

    #[test]
    fn nan_fuel_survives_a_batch() {
        let v = Vitals {
            object: vec![1],
            life: vec![1.0],
            fuel: vec![f32::NAN],
        };
        assert!(Vitals::decode(&v.encode()).unwrap().fuel[0].is_nan());
    }

    #[test]
    fn warehouse_and_airbase_round_trip() {
        let w = Warehouse {
            airbase: "Batumi".into(),
            stock: stock(),
        };
        let b = w.encode();
        assert_eq!(Warehouse::decode(&b), Ok(w));
        truncations_fail(&b, Warehouse::decode);
        let empty = Warehouse {
            airbase: String::new(),
            stock: Lv::Nil,
        };
        assert_eq!(empty.encode().len(), fixed("WAREHOUSE") + 3);

        let a = Airbase {
            object: 12,
            coalition: 2,
            airbase: "Kutaisi".into(),
        };
        let b = a.encode();
        assert_eq!(Airbase::decode(&b), Ok(a));
        truncations_fail(&b, Airbase::decode);
        assert_eq!(Airbase::default().encode().len(), fixed("AIRBASE") + 2);
    }

    #[test]
    fn player_facing_records_follow_their_golden_layouts() {
        let m = Menu {
            entity: 1,
            target: 2,
            scope: 3,
            player: 4,
        };
        at_golden_offsets(
            "MENU",
            &m.encode(),
            &[("entity", 1), ("target", 2), ("scope", 3), ("player", 4)],
        );
        assert_eq!(m.encode().len(), fixed("MENU"));
        let b = m.encode();
        assert_eq!(Menu::decode(&b), Ok(m));
        truncations_fail(&b, Menu::decode);

        let p = Player {
            event: 1,
            player: 2,
            side: 3,
            seat: 4,
            client: "c".into(),
            name: "n".into(),
            slot: "s".into(),
            text: "hello".into(),
        };
        at_golden_offsets(
            "PLAYER",
            &p.encode(),
            &[("event", 1), ("player", 2), ("side", 3), ("seat", 4)],
        );
        let b = p.encode();
        assert_eq!(b.len(), fixed("PLAYER") + 4 * 2 + 1 + 1 + 1 + 5);
        assert_eq!(Player::decode(&b), Ok(p));
        truncations_fail(&b, Player::decode);
        let names: Vec<_> = golden::layout("PLAYER")
            .unwrap()
            .variable
            .iter()
            .map(|v| v.name)
            .collect();
        assert_eq!(names, ["client", "name", "slot", "text"]);

        let e = ExtensionStats {
            calls: 1,
            total_us: 2,
            max_us: 3,
            rev: 4,
            events_skipped: 5,
            stopped: 1,
            name: "count".into(),
        };
        at_golden_offsets(
            "EXTENSION",
            &e.encode(),
            &[
                ("calls", 1),
                ("total_us", 2),
                ("max_us", 3),
                ("rev", 4),
                ("events_skipped", 5),
                ("stopped", 1),
            ],
        );
        let b = e.encode();
        assert_eq!(b.len(), fixed("EXTENSION") + 2 + 5);
        assert_eq!(ExtensionStats::decode(&b), Ok(e));
        truncations_fail(&b, ExtensionStats::decode);

        let a = AuthorEvent {
            objects: vec![7, 8],
            name: "tick".into(),
            payload: Lv::Number(2.0),
        };
        let b = a.encode();
        assert_eq!(&b[..4], &2u32.to_le_bytes());
        assert_eq!(&b[4..8], &[0; 4]);
        assert_eq!(&b[8..16], &7u64.to_le_bytes());
        assert_eq!(AuthorEvent::decode(&b), Ok(a));
        truncations_fail(&b, AuthorEvent::decode);
        let empty = AuthorEvent {
            objects: vec![],
            name: String::new(),
            payload: Lv::Nil,
        };
        assert_eq!(empty.encode().len(), fixed("AUTHOR_EVENT") + 3);
        let names: Vec<_> = golden::layout("AUTHOR_EVENT")
            .unwrap()
            .variable
            .iter()
            .map(|v| (v.ty, v.name))
            .collect();
        assert_eq!(
            names,
            [("u64[n]", "object"), ("str", "name"), ("lv", "payload")]
        );
    }

    #[test]
    fn an_author_event_object_count_past_the_payload_fails() {
        let mut w = Writer::new();
        w.u32(u32::MAX).pad(4).u64(1);
        assert!(AuthorEvent::decode(&w.finish()).is_err());
    }

    #[test]
    fn command_payloads_round_trip() {
        let r = Rates {
            position_s: 1.0,
            vitals_s: 2.0,
            warehouse_s: 4.0,
            enumeration_s: 5.0,
        };
        let b = r.encode();
        assert_eq!(b.len(), fixed("RATES"));
        assert_eq!(Rates::decode(&b), Ok(r));
        truncations_fail(&b, Rates::decode);

        let s = Subscribe {
            period_s: 0.5,
            attitude: 1,
            entity: vec![10, 20],
        };
        let b = s.encode();
        assert_eq!(b.len(), fixed("SUBSCRIBE") + 16);
        assert_eq!(Subscribe::decode(&b), Ok(s));
        truncations_fail(&b, Subscribe::decode);
        assert_eq!(Subscribe::default().encode().len(), fixed("SUBSCRIBE"));

        let bind = Bind {
            kind: 3,
            entity: vec![1, 2],
            object: vec![30, 40],
        };
        let b = bind.encode();
        assert_eq!(b.len(), fixed("BIND") + 32);
        assert_eq!(Bind::decode(&b), Ok(bind));
        truncations_fail(&b, Bind::decode);
        assert_eq!(Bind::default().encode().len(), fixed("BIND"));
    }

    #[test]
    fn command_payloads_follow_a_header() {
        let mut w = Writer::new();
        CommandHeader::default().write(&mut w);
        w.f64(1.0).u32(0).u32(1).u64(77);
        let bytes = w.finish();
        let mut rd = Reader::new(&bytes);
        CommandHeader::read(&mut rd).unwrap();
        let s = Subscribe::decode(&bytes[rd.position()..]).unwrap();
        assert_eq!(s.entity, vec![77]);
    }

    #[test]
    fn a_huge_count_is_an_error_that_allocates_nothing() {
        let mut batch = Writer::new();
        batch.u32(u32::MAX).pad(4).u64(1);
        let batch = batch.finish();
        assert!(Positions::decode(&batch).is_err());
        assert!(Vitals::decode(&batch).is_err());
        assert!(Attitude::decode(&batch).is_err());

        let mut sub = Writer::new();
        sub.f64(1.0).u32(0).u32(u32::MAX).u64(1);
        assert!(Subscribe::decode(&sub.finish()).is_err());

        let mut bind = Writer::new();
        bind.u32(u32::MAX).u32(1).u64(1).u64(2);
        assert!(Bind::decode(&bind.finish()).is_err());
    }

    #[test]
    fn a_count_one_row_too_large_is_an_error() {
        let mut p = Positions {
            object: vec![1],
            x: vec![1.0],
            y: vec![1.0],
            z: vec![1.0],
        }
        .encode();
        p[0] = 2;
        assert!(Positions::decode(&p).is_err());
    }
}
