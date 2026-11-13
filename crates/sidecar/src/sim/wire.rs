//! The records and commands the reconciler reads and writes, typed, with their
//! layouts from spec/records.md and spec/commands.md.
//!
//! The control records (`FRAME`, `HELLO`, `LIFECYCLE`, `RESULT`, `GAP`) and
//! the command header come from `nanoprobe_transport::records`. The object
//! table's records and the two sample batches the reconciler reads are
//! encoded here until the transport crate carries them.

use nanoprobe_transport::codec::{DecodeError, Lv, Reader, Writer};
use nanoprobe_transport::layout::{command, kind};
use nanoprobe_transport::records::{CommandHeader, CommandResult, Frame, Gap, Hello, Lifecycle};

/// Opcodes (spec/commands.md, "Codes: opcode") the reconciler writes.
pub mod opcode {
    /// `CAMPAIGN`
    pub const CAMPAIGN: u16 = 1;
    /// `ENUMERATE`
    pub const ENUMERATE: u16 = 2;
    /// `BURST`
    pub const BURST: u16 = 6;
    /// `GROUP_SPAWN`
    pub const GROUP_SPAWN: u16 = 16;
    /// `GROUP_ORDER`
    pub const GROUP_ORDER: u16 = 17;
    /// `GROUP_DESTROY`
    pub const GROUP_DESTROY: u16 = 18;
    /// `GROUP_CAPTURE_DESTROY`
    pub const GROUP_CAPTURE_DESTROY: u16 = 19;
    /// `STATIC_SPAWN`
    pub const STATIC_SPAWN: u16 = 21;
    /// `STATIC_DESTROY`
    pub const STATIC_DESTROY: u16 = 22;
    /// `CAPTURE`
    pub const CAPTURE: u16 = 64;
    /// Every opcode here, by name, for the golden check.
    pub const ALL: &[(&str, u16)] = &[
        ("CAMPAIGN", CAMPAIGN),
        ("ENUMERATE", ENUMERATE),
        ("BURST", BURST),
        ("GROUP_SPAWN", GROUP_SPAWN),
        ("GROUP_ORDER", GROUP_ORDER),
        ("GROUP_DESTROY", GROUP_DESTROY),
        ("GROUP_CAPTURE_DESTROY", GROUP_CAPTURE_DESTROY),
        ("STATIC_SPAWN", STATIC_SPAWN),
        ("STATIC_DESTROY", STATIC_DESTROY),
        ("CAPTURE", CAPTURE),
    ];
}

/// `object_kind` codes.
pub mod object_kind {
    /// `UNIT`
    pub const UNIT: u32 = 1;
    /// `STATIC`
    pub const STATIC: u32 = 2;
}

/// `gone_reason` codes.
pub mod gone_reason {
    /// A death event named it.
    pub const DEAD: u32 = 1;
    /// The agent destroyed it for a command.
    pub const DESTROYED: u32 = 2;
    /// A full enumeration did not find it.
    pub const MISSING: u32 = 3;
    /// A removal event named it.
    pub const REMOVED: u32 = 4;
}

/// `OBJECT`: a unit or static entered the agent's table, or an enumeration
/// found it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Object {
    /// DCS object ID.
    pub object: u64,
    /// DCS group ID, 0 for a static.
    pub group: u64,
    /// The binding's `EntityId` for the object, 0 for none.
    pub entity: u64,
    /// The binding's `EntityId` for the group, 0 for none.
    pub group_entity: u64,
    /// The enumeration that found it, 0 for an event or a spawn.
    pub enumeration: u64,
    /// `object_kind`.
    pub kind: u32,
    /// DCS category.
    pub category: u32,
    /// DCS coalition.
    pub coalition: u32,
    /// DCS country.
    pub country: u32,
    /// Initial life.
    pub life0: f32,
    /// DCS name.
    pub name: String,
    /// DCS type name.
    pub type_name: String,
    /// The group's DCS name, empty for a static.
    pub group_name: String,
}

impl Object {
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
        let mut o = Object {
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
            ..Object::default()
        };
        r.skip(4)?;
        o.name = r.str()?.to_owned();
        o.type_name = r.str()?.to_owned();
        o.group_name = r.str()?.to_owned();
        Ok(o)
    }
}

/// `OBJECT_GONE`: a unit or static left the agent's table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ObjectGone {
    /// DCS object ID.
    pub object: u64,
    /// `gone_reason`.
    pub reason: u32,
}

impl ObjectGone {
    /// The payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u64(self.object).u32(self.reason).pad(4);
        w.finish()
    }

    /// Decode a payload.
    pub fn decode(b: &[u8]) -> Result<ObjectGone, DecodeError> {
        let mut r = Reader::new(b);
        let g = ObjectGone {
            object: r.u64()?,
            reason: r.u32()?,
        };
        r.skip(4)?;
        Ok(g)
    }
}

/// `ENUM_BEGIN` and `ENUM_END`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Enumeration {
    /// The enumeration, never 0.
    pub enumeration: u64,
    /// `OBJECT` records it wrote; `ENUM_END` only.
    pub objects: u32,
}

impl Enumeration {
    /// The `ENUM_BEGIN` payload.
    pub fn encode_begin(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u64(self.enumeration);
        w.finish()
    }

    /// The `ENUM_END` payload.
    pub fn encode_end(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u64(self.enumeration).u32(self.objects).pad(4);
        w.finish()
    }

    /// Decode an `ENUM_BEGIN` payload.
    pub fn decode_begin(b: &[u8]) -> Result<Enumeration, DecodeError> {
        let mut r = Reader::new(b);
        Ok(Enumeration {
            enumeration: r.u64()?,
            objects: 0,
        })
    }

    /// Decode an `ENUM_END` payload.
    pub fn decode_end(b: &[u8]) -> Result<Enumeration, DecodeError> {
        let mut r = Reader::new(b);
        let e = Enumeration {
            enumeration: r.u64()?,
            objects: r.u32()?,
        };
        r.skip(4)?;
        Ok(e)
    }
}

/// One row of a `POSITIONS` batch.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Position {
    /// DCS object ID.
    pub object: u64,
    /// DCS `x`.
    pub x: f64,
    /// DCS `y`.
    pub y: f64,
    /// DCS `z`.
    pub z: f64,
}

/// One row of a `VITALS` batch.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vitals {
    /// DCS object ID.
    pub object: u64,
    /// `getLife()`.
    pub life: f32,
    /// `getFuel()`.
    pub fuel: f32,
}

fn rows(r: &mut Reader<'_>, width: usize) -> Result<usize, DecodeError> {
    let n = r.u32()? as usize;
    r.skip(4)?;
    if n.checked_mul(width).is_none_or(|b| b > r.remaining()) {
        return Err(DecodeError {
            at: r.position(),
            what: "rows past the payload",
        });
    }
    Ok(n)
}

/// Encode a `POSITIONS` batch.
pub fn encode_positions(rows: &[Position]) -> Vec<u8> {
    let mut w = Writer::new();
    w.u32(rows.len() as u32).pad(4);
    rows.iter().for_each(|p| {
        w.u64(p.object);
    });
    rows.iter().for_each(|p| {
        w.f64(p.x);
    });
    rows.iter().for_each(|p| {
        w.f64(p.y);
    });
    rows.iter().for_each(|p| {
        w.f64(p.z);
    });
    w.finish()
}

/// Decode a `POSITIONS` batch.
pub fn decode_positions(b: &[u8]) -> Result<Vec<Position>, DecodeError> {
    let mut r = Reader::new(b);
    let n = rows(&mut r, 32)?;
    let mut out = vec![Position::default(); n];
    for p in &mut out {
        p.object = r.u64()?;
    }
    for p in &mut out {
        p.x = r.f64()?;
    }
    for p in &mut out {
        p.y = r.f64()?;
    }
    for p in &mut out {
        p.z = r.f64()?;
    }
    Ok(out)
}

/// Encode a `VITALS` batch.
pub fn encode_vitals(rows: &[Vitals]) -> Vec<u8> {
    let mut w = Writer::new();
    w.u32(rows.len() as u32).pad(4);
    rows.iter().for_each(|v| {
        w.u64(v.object);
    });
    rows.iter().for_each(|v| {
        w.f32(v.life);
    });
    rows.iter().for_each(|v| {
        w.f32(v.fuel);
    });
    w.finish()
}

/// Decode a `VITALS` batch.
pub fn decode_vitals(b: &[u8]) -> Result<Vec<Vitals>, DecodeError> {
    let mut r = Reader::new(b);
    let n = rows(&mut r, 16)?;
    let mut out = vec![Vitals::default(); n];
    for v in &mut out {
        v.object = r.u64()?;
    }
    for v in &mut out {
        v.life = r.f32()?;
    }
    for v in &mut out {
        v.fuel = r.f32()?;
    }
    Ok(out)
}

/// One outbound record, decoded. Kinds the reconciler does not read are
/// `Other`.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// `FRAME`
    Frame(Frame),
    /// `HELLO`
    Hello(Hello),
    /// `LIFECYCLE`
    Lifecycle(Lifecycle),
    /// `RESULT`
    Result(CommandResult),
    /// `GAP`
    Gap(Gap),
    /// `OBJECT`
    Object(Object),
    /// `OBJECT_GONE`
    ObjectGone(ObjectGone),
    /// `ENUM_BEGIN`
    EnumBegin(Enumeration),
    /// `ENUM_END`
    EnumEnd(Enumeration),
    /// `POSITIONS`
    Positions(Vec<Position>),
    /// `VITALS`
    Vitals(Vec<Vitals>),
    /// `EVENT`, undecoded: events are hints to a level-triggered reconciler.
    Event(Vec<u8>),
    /// Any other kind.
    Other(u16),
}

impl Input {
    /// Decode a record by its kind.
    pub fn decode(record_kind: u16, payload: &[u8]) -> Result<Input, DecodeError> {
        Ok(match u64::from(record_kind) {
            kind::FRAME => Input::Frame(Frame::decode(payload)?),
            kind::HELLO => Input::Hello(Hello::decode(payload)?),
            kind::LIFECYCLE => Input::Lifecycle(Lifecycle::decode(payload)?),
            kind::RESULT => Input::Result(CommandResult::decode(payload)?),
            kind::GAP => Input::Gap(Gap::decode(payload)?),
            kind::OBJECT => Input::Object(Object::decode(payload)?),
            kind::OBJECT_GONE => Input::ObjectGone(ObjectGone::decode(payload)?),
            kind::ENUM_BEGIN => Input::EnumBegin(Enumeration::decode_begin(payload)?),
            kind::ENUM_END => Input::EnumEnd(Enumeration::decode_end(payload)?),
            kind::POSITIONS => Input::Positions(decode_positions(payload)?),
            kind::VITALS => Input::Vitals(decode_vitals(payload)?),
            kind::EVENT => Input::Event(payload.to_vec()),
            _ => Input::Other(record_kind),
        })
    }

    /// The record's kind and payload.
    pub fn encode(&self) -> (u16, Vec<u8>) {
        let (k, p) = match self {
            Input::Frame(f) => (kind::FRAME, f.encode()),
            Input::Hello(h) => (kind::HELLO, h.encode()),
            Input::Lifecycle(l) => (kind::LIFECYCLE, l.encode()),
            Input::Result(r) => (kind::RESULT, r.encode()),
            Input::Gap(g) => (kind::GAP, g.encode()),
            Input::Object(o) => (kind::OBJECT, o.encode()),
            Input::ObjectGone(g) => (kind::OBJECT_GONE, g.encode()),
            Input::EnumBegin(e) => (kind::ENUM_BEGIN, e.encode_begin()),
            Input::EnumEnd(e) => (kind::ENUM_END, e.encode_end()),
            Input::Positions(p) => (kind::POSITIONS, encode_positions(p)),
            Input::Vitals(v) => (kind::VITALS, encode_vitals(v)),
            Input::Event(b) => (kind::EVENT, b.clone()),
            Input::Other(k) => (u64::from(*k), Vec::new()),
        };
        (k as u16, p)
    }
}

/// What a command carries after its header.
#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    /// `CAMPAIGN`
    Campaign(u64),
    /// `ENUMERATE`
    Enumerate,
    /// `BURST`
    Burst(bool),
    /// `GROUP_SPAWN`
    GroupSpawn {
        /// DCS country.
        country: u32,
        /// DCS group category.
        category: u32,
        /// The group's platform name.
        name: String,
        /// The group table.
        group: Lv,
    },
    /// `GROUP_ORDER`
    GroupOrder(Lv),
    /// `GROUP_DESTROY`
    GroupDestroy,
    /// `GROUP_CAPTURE_DESTROY`
    GroupCaptureDestroy,
    /// `STATIC_SPAWN`
    StaticSpawn {
        /// DCS country.
        country: u32,
        /// The static's platform name.
        name: String,
        /// The static table.
        table: Lv,
    },
    /// `STATIC_DESTROY`
    StaticDestroy,
    /// `CAPTURE`
    Capture,
}

impl Body {
    /// The opcode.
    pub fn opcode(&self) -> u16 {
        match self {
            Body::Campaign(_) => opcode::CAMPAIGN,
            Body::Enumerate => opcode::ENUMERATE,
            Body::Burst(_) => opcode::BURST,
            Body::GroupSpawn { .. } => opcode::GROUP_SPAWN,
            Body::GroupOrder(_) => opcode::GROUP_ORDER,
            Body::GroupDestroy => opcode::GROUP_DESTROY,
            Body::GroupCaptureDestroy => opcode::GROUP_CAPTURE_DESTROY,
            Body::StaticSpawn { .. } => opcode::STATIC_SPAWN,
            Body::StaticDestroy => opcode::STATIC_DESTROY,
            Body::Capture => opcode::CAPTURE,
        }
    }

    /// Whether the opcode is a control command: no sequence number, no epoch.
    pub fn is_control(&self) -> bool {
        matches!(self, Body::Campaign(_) | Body::Enumerate | Body::Burst(_))
    }
}

/// A command, header and body.
#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    /// The header.
    pub header: CommandHeader,
    /// The body.
    pub body: Body,
}

impl Command {
    /// The opcode and the whole payload.
    pub fn encode(&self) -> (u16, Vec<u8>) {
        let mut w = Writer::new();
        self.header.write(&mut w);
        match &self.body {
            Body::Campaign(c) => {
                w.u64(*c);
            }
            Body::Burst(on) => {
                w.u32(u32::from(*on)).pad(4);
            }
            Body::GroupSpawn {
                country,
                category,
                name,
                group,
            } => {
                w.u32(*country).u32(*category).str(name).lv(group);
            }
            Body::GroupOrder(task) => {
                w.lv(task);
            }
            Body::StaticSpawn {
                country,
                name,
                table,
            } => {
                w.u32(*country).pad(4).str(name).lv(table);
            }
            Body::Enumerate
            | Body::GroupDestroy
            | Body::GroupCaptureDestroy
            | Body::StaticDestroy
            | Body::Capture => {}
        }
        (self.body.opcode(), w.finish())
    }

    /// Decode a command. `None` for an opcode this module does not carry.
    pub fn decode(op: u16, payload: &[u8]) -> Result<Option<Command>, DecodeError> {
        if payload.len() < command::HEADER {
            return Err(DecodeError {
                at: payload.len(),
                what: "payload shorter than the command header",
            });
        }
        let mut r = Reader::new(payload);
        let header = CommandHeader::read(&mut r)?;
        let body = match op {
            opcode::CAMPAIGN => Body::Campaign(r.u64()?),
            opcode::ENUMERATE => Body::Enumerate,
            opcode::BURST => {
                let on = r.u32()? != 0;
                r.skip(4)?;
                Body::Burst(on)
            }
            opcode::GROUP_SPAWN => Body::GroupSpawn {
                country: r.u32()?,
                category: r.u32()?,
                name: r.str()?.to_owned(),
                group: r.lv()?,
            },
            opcode::GROUP_ORDER => Body::GroupOrder(r.lv()?),
            opcode::GROUP_DESTROY => Body::GroupDestroy,
            opcode::GROUP_CAPTURE_DESTROY => Body::GroupCaptureDestroy,
            opcode::STATIC_SPAWN => {
                let country = r.u32()?;
                r.skip(4)?;
                Body::StaticSpawn {
                    country,
                    name: r.str()?.to_owned(),
                    table: r.lv()?,
                }
            }
            opcode::STATIC_DESTROY => Body::StaticDestroy,
            opcode::CAPTURE => Body::Capture,
            _ => return Ok(None),
        };
        Ok(Some(Command { header, body }))
    }
}

/// Helpers for building and reading the `Lv` tables the commands carry.
pub mod lv {
    use super::Lv;

    /// A string value.
    pub fn s(v: &str) -> Lv {
        Lv::String(v.as_bytes().to_vec())
    }

    /// A number value.
    pub fn n(v: f64) -> Lv {
        Lv::Number(v)
    }

    /// A table with string keys.
    pub fn obj(pairs: Vec<(&str, Lv)>) -> Lv {
        Lv::Table(pairs.into_iter().map(|(k, v)| (s(k), v)).collect())
    }

    /// A table with keys 1 to n.
    pub fn list(items: Vec<Lv>) -> Lv {
        Lv::Table(
            items
                .into_iter()
                .enumerate()
                .map(|(i, v)| (Lv::Number((i + 1) as f64), v))
                .collect(),
        )
    }

    /// The value at a string key of a table.
    pub fn get<'a>(t: &'a Lv, key: &str) -> Option<&'a Lv> {
        match t {
            Lv::Table(pairs) => pairs
                .iter()
                .find(|(k, _)| matches!(k, Lv::String(b) if b == key.as_bytes()))
                .map(|(_, v)| v),
            _ => None,
        }
    }

    /// The values of a list, in key order.
    pub fn items(t: &Lv) -> Vec<&Lv> {
        match t {
            Lv::Table(pairs) => {
                let mut v: Vec<(f64, &Lv)> = pairs
                    .iter()
                    .filter_map(|(k, v)| match k {
                        Lv::Number(i) => Some((*i, v)),
                        _ => None,
                    })
                    .collect();
                v.sort_by(|a, b| a.0.total_cmp(&b.0));
                v.into_iter().map(|(_, v)| v).collect()
            }
            _ => Vec::new(),
        }
    }

    /// A string at a key.
    pub fn str_at(t: &Lv, key: &str) -> Option<String> {
        match get(t, key)? {
            Lv::String(b) => String::from_utf8(b.clone()).ok(),
            _ => None,
        }
    }

    /// A number at a key.
    pub fn num_at(t: &Lv, key: &str) -> Option<f64> {
        match get(t, key)? {
            Lv::Number(n) => Some(*n),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanoprobe_transport::golden;

    #[test]
    fn opcodes_match_the_golden_file() {
        for (name, code) in opcode::ALL {
            assert_eq!(golden::code_name("opcode", u64::from(*code)), Some(*name));
        }
        assert_eq!(
            golden::code_name("object_kind", u64::from(object_kind::UNIT)),
            Some("UNIT")
        );
        assert_eq!(
            golden::code_name("object_kind", u64::from(object_kind::STATIC)),
            Some("STATIC")
        );
        for (name, code) in [
            ("DEAD", gone_reason::DEAD),
            ("DESTROYED", gone_reason::DESTROYED),
            ("MISSING", gone_reason::MISSING),
            ("REMOVED", gone_reason::REMOVED),
        ] {
            assert_eq!(
                golden::code_name("gone_reason", u64::from(code)),
                Some(name)
            );
        }
    }

    #[test]
    fn fixed_parts_match_the_golden_layouts() {
        for (name, len) in [
            ("OBJECT", Object::default().encode().len() - 6),
            ("OBJECT_GONE", ObjectGone::default().encode().len()),
            ("ENUM_BEGIN", Enumeration::default().encode_begin().len()),
            ("ENUM_END", Enumeration::default().encode_end().len()),
        ] {
            assert_eq!(golden::layout(name).unwrap().fixed, len, "{name}");
        }
    }

    #[test]
    fn records_round_trip() {
        let inputs = vec![
            Input::Object(Object {
                object: 7,
                group: 3,
                entity: 9,
                group_entity: 8,
                enumeration: 2,
                kind: 1,
                category: 2,
                coalition: 2,
                country: 0,
                life0: 3.5,
                name: "np.u.1.9".into(),
                type_name: "M-113".into(),
                group_name: "np.g.1.8".into(),
            }),
            Input::ObjectGone(ObjectGone {
                object: 7,
                reason: 3,
            }),
            Input::EnumBegin(Enumeration {
                enumeration: 4,
                objects: 0,
            }),
            Input::EnumEnd(Enumeration {
                enumeration: 4,
                objects: 11,
            }),
            Input::Positions(vec![
                Position {
                    object: 1,
                    x: 1.0,
                    y: 2.0,
                    z: 3.0,
                },
                Position {
                    object: 2,
                    x: -1.0,
                    y: 0.5,
                    z: 9.0,
                },
            ]),
            Input::Vitals(vec![Vitals {
                object: 1,
                life: 1.0,
                fuel: 0.5,
            }]),
        ];
        for i in inputs {
            let (k, p) = i.encode();
            assert_eq!(Input::decode(k, &p).unwrap(), i);
        }
    }

    #[test]
    fn short_and_long_row_counts_fail() {
        let mut p = encode_positions(&[Position::default()]);
        p[0] = 2;
        assert!(decode_positions(&p).is_err());
        assert!(decode_vitals(&[0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0]).is_err());
    }

    #[test]
    fn commands_round_trip() {
        let h = CommandHeader {
            seq: 5,
            epoch: 2,
            campaign: 9,
            entity: 4,
            rev: 1,
            mission_gen: 3,
        };
        for body in [
            Body::Campaign(9),
            Body::Enumerate,
            Body::Burst(true),
            Body::GroupSpawn {
                country: 2,
                category: 2,
                name: "np.g.9.4".into(),
                group: lv::obj(vec![("name", lv::s("np.g.9.4"))]),
            },
            Body::GroupOrder(lv::obj(vec![("id", lv::s("Hold"))])),
            Body::GroupDestroy,
            Body::GroupCaptureDestroy,
            Body::StaticSpawn {
                country: 2,
                name: "np.s.9.4".into(),
                table: lv::obj(vec![("type", lv::s("Tent"))]),
            },
            Body::StaticDestroy,
            Body::Capture,
        ] {
            let c = Command { header: h, body };
            let (op, p) = c.encode();
            assert_eq!(Command::decode(op, &p).unwrap(), Some(c));
        }
        assert!(Command::decode(16, &[0; 10]).is_err());
        assert_eq!(Command::decode(49, &[0; 48]).unwrap(), None);
    }
}
