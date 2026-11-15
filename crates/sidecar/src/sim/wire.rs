//! The records and commands the reconciler reads and writes, typed, with their
//! layouts from spec/records.md and spec/commands.md.
//!
//! Every record's codec is `nanoprobe_transport::records`'. This module
//! gives the reconciler its inputs as one enum, with the two sample batches
//! it reads as rows.

use nanoprobe_transport::codec::{DecodeError, Lv, Reader, Writer};
use nanoprobe_transport::layout::{command, kind};
use nanoprobe_transport::records::{
    Airbase, AuthorEvent, Bind, CommandHeader, CommandResult, EnumBegin, EnumEnd, Event,
    ExtensionStats, Frame, Gap, Hello, Lifecycle, Menu, Player, Positions, Rates, Subscribe,
    Vitals as TransportVitals, Warehouse,
};

/// Opcodes (spec/commands.md, "Codes: opcode") the reconciler writes.
pub mod opcode {
    /// `CAMPAIGN`
    pub const CAMPAIGN: u16 = 1;
    /// `ENUMERATE`
    pub const ENUMERATE: u16 = 2;
    /// `RATES`
    pub const RATES: u16 = 3;
    /// `SUBSCRIBE`
    pub const SUBSCRIBE: u16 = 4;
    /// `BIND`
    pub const BIND: u16 = 5;
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
    /// `GROUP_OPTION`
    pub const GROUP_OPTION: u16 = 20;
    /// `STATIC_SPAWN`
    pub const STATIC_SPAWN: u16 = 21;
    /// `STATIC_DESTROY`
    pub const STATIC_DESTROY: u16 = 22;
    /// `AIRBASE_OWNER`
    pub const AIRBASE_OWNER: u16 = 23;
    /// `WAREHOUSE_SET`
    pub const WAREHOUSE_SET: u16 = 24;
    /// `MARK_SET`
    pub const MARK_SET: u16 = 25;
    /// `MARK_REMOVE`
    pub const MARK_REMOVE: u16 = 26;
    /// `DRAWING_SET`
    pub const DRAWING_SET: u16 = 27;
    /// `DRAWING_REMOVE`
    pub const DRAWING_REMOVE: u16 = 28;
    /// `MENU_SET`
    pub const MENU_SET: u16 = 29;
    /// `MENU_REMOVE`
    pub const MENU_REMOVE: u16 = 30;
    /// `EXTENSION_SET`
    pub const EXTENSION_SET: u16 = 31;
    /// `EXTENSION_REMOVE`
    pub const EXTENSION_REMOVE: u16 = 32;
    /// `MESSAGE`
    pub const MESSAGE: u16 = 49;
    /// `SOUND`
    pub const SOUND: u16 = 50;
    /// `DAMAGE`
    pub const DAMAGE: u16 = 54;
    /// `CHAT`
    pub const CHAT: u16 = 55;
    /// `MISSION_LOAD`
    pub const MISSION_LOAD: u16 = 56;
    /// `EXTENSION_INVOKE`
    pub const EXTENSION_INVOKE: u16 = 57;
    /// `EVAL`
    pub const EVAL: u16 = 58;
    /// `MODERATE`
    pub const MODERATE: u16 = 62;
    /// `PLAYERS`
    pub const PLAYERS: u16 = 66;
    /// `BANS`
    pub const BANS: u16 = 67;
    /// `WAREHOUSE_ADJUST`
    pub const WAREHOUSE_ADJUST: u16 = 59;
    /// `CAPTURE`
    pub const CAPTURE: u16 = 64;
    /// `PICTURE`
    pub const PICTURE: u16 = 60;
    /// `PICTURE_STOP`
    pub const PICTURE_STOP: u16 = 61;
    /// `COUNTRIES`
    pub const COUNTRIES: u16 = 65;
    /// Every opcode here, by name, for the golden check.
    pub const ALL: &[(&str, u16)] = &[
        ("CAMPAIGN", CAMPAIGN),
        ("ENUMERATE", ENUMERATE),
        ("RATES", RATES),
        ("SUBSCRIBE", SUBSCRIBE),
        ("BURST", BURST),
        ("GROUP_SPAWN", GROUP_SPAWN),
        ("GROUP_ORDER", GROUP_ORDER),
        ("GROUP_DESTROY", GROUP_DESTROY),
        ("GROUP_CAPTURE_DESTROY", GROUP_CAPTURE_DESTROY),
        ("GROUP_OPTION", GROUP_OPTION),
        ("STATIC_SPAWN", STATIC_SPAWN),
        ("STATIC_DESTROY", STATIC_DESTROY),
        ("AIRBASE_OWNER", AIRBASE_OWNER),
        ("WAREHOUSE_SET", WAREHOUSE_SET),
        ("MARK_SET", MARK_SET),
        ("MARK_REMOVE", MARK_REMOVE),
        ("DRAWING_SET", DRAWING_SET),
        ("DRAWING_REMOVE", DRAWING_REMOVE),
        ("MENU_SET", MENU_SET),
        ("MENU_REMOVE", MENU_REMOVE),
        ("EXTENSION_SET", EXTENSION_SET),
        ("EXTENSION_REMOVE", EXTENSION_REMOVE),
        ("MESSAGE", MESSAGE),
        ("SOUND", SOUND),
        ("DAMAGE", DAMAGE),
        ("CHAT", CHAT),
        ("MISSION_LOAD", MISSION_LOAD),
        ("EXTENSION_INVOKE", EXTENSION_INVOKE),
        ("EVAL", EVAL),
        ("MODERATE", MODERATE),
        ("PLAYERS", PLAYERS),
        ("BANS", BANS),
        ("WAREHOUSE_ADJUST", WAREHOUSE_ADJUST),
        ("PICTURE", PICTURE),
        ("PICTURE_STOP", PICTURE_STOP),
        ("CAPTURE", CAPTURE),
        ("COUNTRIES", COUNTRIES),
    ];
}

/// `object_kind` codes.
pub mod object_kind {
    /// `UNIT`
    pub const UNIT: u32 = 1;
    /// `STATIC`
    pub const STATIC: u32 = 2;
    /// `GROUP`, in `BIND` only.
    pub const GROUP: u32 = 3;
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

pub use nanoprobe_transport::records::{Object, ObjectGone, Seen};

/// `ENUM_BEGIN` and `ENUM_END`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Enumeration {
    /// The enumeration, never 0.
    pub enumeration: u64,
    /// `OBJECT` records it wrote; `ENUM_END` only.
    pub objects: u32,
    /// IDs it wrote in `SEEN` records; `ENUM_END` only.
    pub seen: u32,
}

impl Enumeration {
    /// The `ENUM_BEGIN` payload.
    pub fn encode_begin(&self) -> Vec<u8> {
        EnumBegin {
            enumeration: self.enumeration,
        }
        .encode()
    }

    /// The `ENUM_END` payload.
    pub fn encode_end(&self) -> Vec<u8> {
        EnumEnd {
            enumeration: self.enumeration,
            objects: self.objects,
            seen: self.seen,
        }
        .encode()
    }

    /// Decode an `ENUM_BEGIN` payload.
    pub fn decode_begin(b: &[u8]) -> Result<Enumeration, DecodeError> {
        Ok(Enumeration {
            enumeration: EnumBegin::decode(b)?.enumeration,
            objects: 0,
            seen: 0,
        })
    }

    /// Decode an `ENUM_END` payload.
    pub fn decode_end(b: &[u8]) -> Result<Enumeration, DecodeError> {
        let e = EnumEnd::decode(b)?;
        Ok(Enumeration {
            enumeration: e.enumeration,
            objects: e.objects,
            seen: e.seen,
        })
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

/// Encode a `POSITIONS` batch.
pub fn encode_positions(rows: &[Position]) -> Vec<u8> {
    Positions {
        object: rows.iter().map(|p| p.object).collect(),
        x: rows.iter().map(|p| p.x).collect(),
        y: rows.iter().map(|p| p.y).collect(),
        z: rows.iter().map(|p| p.z).collect(),
    }
    .encode()
}

/// Decode a `POSITIONS` batch.
pub fn decode_positions(b: &[u8]) -> Result<Vec<Position>, DecodeError> {
    let p = Positions::decode(b)?;
    Ok((0..p.object.len())
        .map(|i| Position {
            object: p.object[i],
            x: p.x[i],
            y: p.y[i],
            z: p.z[i],
        })
        .collect())
}

/// Encode a `VITALS` batch.
pub fn encode_vitals(rows: &[Vitals]) -> Vec<u8> {
    TransportVitals {
        object: rows.iter().map(|v| v.object).collect(),
        life: rows.iter().map(|v| v.life).collect(),
        fuel: rows.iter().map(|v| v.fuel).collect(),
    }
    .encode()
}

/// Decode a `VITALS` batch.
pub fn decode_vitals(b: &[u8]) -> Result<Vec<Vitals>, DecodeError> {
    let v = TransportVitals::decode(b)?;
    Ok((0..v.object.len())
        .map(|i| Vitals {
            object: v.object[i],
            life: v.life[i],
            fuel: v.fuel[i],
        })
        .collect())
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
    /// `SEEN`
    Seen(Seen),
    /// `ENUM_BEGIN`
    EnumBegin(Enumeration),
    /// `ENUM_END`
    EnumEnd(Enumeration),
    /// `POSITIONS`
    Positions(Vec<Position>),
    /// `VITALS`
    Vitals(Vec<Vitals>),
    /// `AIRBASE`
    Airbase(Airbase),
    /// `WAREHOUSE`
    Warehouse(Warehouse),
    /// `EVENT`: a hint to a level-triggered reconciler, never a fact.
    Event(Event),
    /// `AUTHOR_EVENT`
    AuthorEvent(AuthorEvent),
    /// `PLAYER`
    Player(Player),
    /// `MENU`
    Menu(Menu),
    /// `EXTENSION`
    Extension(ExtensionStats),
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
            kind::SEEN => Input::Seen(Seen::decode(payload)?),
            kind::ENUM_BEGIN => Input::EnumBegin(Enumeration::decode_begin(payload)?),
            kind::ENUM_END => Input::EnumEnd(Enumeration::decode_end(payload)?),
            kind::POSITIONS => Input::Positions(decode_positions(payload)?),
            kind::VITALS => Input::Vitals(decode_vitals(payload)?),
            kind::EVENT => Input::Event(Event::decode(payload)?),
            kind::AIRBASE => Input::Airbase(Airbase::decode(payload)?),
            kind::WAREHOUSE => Input::Warehouse(Warehouse::decode(payload)?),
            kind::AUTHOR_EVENT => Input::AuthorEvent(AuthorEvent::decode(payload)?),
            kind::PLAYER => Input::Player(Player::decode(payload)?),
            kind::MENU => Input::Menu(Menu::decode(payload)?),
            kind::EXTENSION => Input::Extension(ExtensionStats::decode(payload)?),
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
            Input::Seen(s) => (kind::SEEN, s.encode()),
            Input::EnumBegin(e) => (kind::ENUM_BEGIN, e.encode_begin()),
            Input::EnumEnd(e) => (kind::ENUM_END, e.encode_end()),
            Input::Positions(p) => (kind::POSITIONS, encode_positions(p)),
            Input::Vitals(v) => (kind::VITALS, encode_vitals(v)),
            Input::Event(e) => (kind::EVENT, e.encode()),
            Input::Airbase(a) => (kind::AIRBASE, a.encode()),
            Input::Warehouse(w) => (kind::WAREHOUSE, w.encode()),
            Input::AuthorEvent(a) => (kind::AUTHOR_EVENT, a.encode()),
            Input::Player(p) => (kind::PLAYER, p.encode()),
            Input::Menu(m) => (kind::MENU, m.encode()),
            Input::Extension(e) => (kind::EXTENSION, e.encode()),
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
    /// `RATES`
    Rates(Rates),
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
    /// `GROUP_OPTION`
    GroupOption {
        /// The DCS option ID.
        option: u32,
        /// The option's value.
        value: Lv,
    },
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
    /// `AIRBASE_OWNER`
    AirbaseOwner {
        /// DCS coalition number.
        coalition: u32,
        /// The airbase's DCS name.
        airbase: String,
    },
    /// `WAREHOUSE_SET`
    WarehouseSet {
        /// The airbase's DCS name.
        airbase: String,
        /// Names and counts to set, in the shape of a `WAREHOUSE` record's.
        stock: Lv,
    },
    /// `WAREHOUSE_ADJUST`
    WarehouseAdjust {
        /// The airbase's DCS name.
        airbase: String,
        /// Names and amounts to add; a negative amount takes away.
        delta: Lv,
    },
    /// `DAMAGE`
    Damage {
        /// The life to set, as a percentage of the unit's initial life.
        life_percent: f64,
    },
    /// `SOUND`
    Sound {
        /// A `scope` code.
        scope: u32,
        /// By scope: a coalition or country number, or an `EntityId`.
        target: u64,
        /// The sound file's name in the mission.
        file: String,
    },
    /// `PICTURE`
    Picture {
        /// A `scope` code.
        scope: u32,
        /// By scope, as for `Sound`.
        target: u64,
        /// 1 to clear the view.
        clear_view: u32,
        /// How long the picture stays; 0 until `PICTURE_STOP`.
        seconds: f64,
        /// The picture's size, in `size_units`.
        size: f64,
        /// The Mission Editor's horizontal alignment value.
        horizontal: u32,
        /// The Mission Editor's vertical alignment value.
        vertical: u32,
        /// The Mission Editor's size-units value.
        size_units: u32,
        /// The picture's resource key or file name.
        file: String,
    },
    /// `PICTURE_STOP`
    PictureStop,
    /// `MARK_SET`
    MarkSet {
        /// A `scope` code: `ALL`, `COALITION` or `GROUP`.
        scope: u32,
        /// Whether players cannot change it.
        read_only: bool,
        /// By scope: a coalition number or a group `EntityId`.
        target: u64,
        /// DCS `x`, meters.
        x: f64,
        /// DCS `y`, meters.
        y: f64,
        /// DCS `z`, meters.
        z: f64,
        /// The mark's text.
        text: String,
    },
    /// `MARK_REMOVE`
    MarkRemove,
    /// `DRAWING_SET`
    DrawingSet {
        /// A `scope` code: `ALL` or `COALITION`.
        scope: u32,
        /// The DCS shape ID.
        shape: u32,
        /// The coalition number, by scope.
        target: u64,
        /// The shape's arguments after the coalition and the ID, as an array.
        args: Lv,
    },
    /// `DRAWING_REMOVE`
    DrawingRemove,
    /// `MENU_SET`
    MenuSet {
        /// The parent menu item's `EntityId`, 0 for the root.
        parent: u64,
        /// By scope: a coalition number or a group `EntityId`.
        target: u64,
        /// A `scope` code: `ALL`, `COALITION` or `GROUP`.
        scope: u32,
        /// Whether the item is a submenu rather than a command.
        submenu: bool,
        /// The item's text.
        text: String,
    },
    /// `MENU_REMOVE`
    MenuRemove,
    /// `EXTENSION_SET`
    ExtensionSet {
        /// The extension's name.
        name: String,
        /// Its Lua source, a string.
        source: Lv,
    },
    /// `EXTENSION_REMOVE`
    ExtensionRemove {
        /// The extension's name.
        name: String,
    },
    /// `MESSAGE`
    Message {
        /// A `scope` code.
        scope: u32,
        /// Whether to clear earlier messages.
        clear: bool,
        /// By scope: a coalition or country number, or an `EntityId`.
        target: u64,
        /// How long the text stays, seconds.
        seconds: f64,
        /// The text.
        text: String,
    },
    /// `CHAT`
    Chat {
        /// The DCS connection ID to send to, 0 for everyone.
        player: u32,
        /// The message.
        text: String,
    },
    /// `MISSION_LOAD`
    MissionLoad {
        /// The mission file's path as DCS opens it.
        path: String,
    },
    /// `EVAL`
    Eval {
        /// Lua source, a string.
        source: Lv,
    },
    /// `EXTENSION_INVOKE`
    ExtensionInvoke {
        /// The extension's name.
        name: String,
        /// An array of arguments.
        args: Lv,
    },
    /// `CAPTURE`
    Capture,
    /// `COUNTRIES`
    Countries,
    /// `SUBSCRIBE`
    Subscribe(Subscribe),
    /// `BIND`
    Bind(Bind),
    /// `MODERATE`
    Moderate {
        /// A `moderation` code (see [`moderation`]).
        action: u32,
        /// The DCS connection ID, 0 for none.
        player: u32,
        /// The coalition to move to, for `SLOT`; 0 for spectators.
        side: u32,
        /// How long a `BAN` lasts, 0 for DCS's default.
        seconds: f64,
        /// The client's ucid. It goes to the agent and no further.
        ucid: String,
        /// The slot ID to move to, for `SLOT`; empty for spectators.
        slot: String,
        /// Shown to the player, for `KICK` and `BAN`.
        reason: String,
    },
    /// `PLAYERS`
    Players,
    /// `BANS`
    Bans,
}

/// `moderation` codes (spec/commands.md, "Codes: moderation").
pub mod moderation {
    /// `KICK`
    pub const KICK: u32 = 1;
    /// `BAN`
    pub const BAN: u32 = 2;
    /// `UNBAN`
    pub const UNBAN: u32 = 3;
    /// `SLOT`
    pub const SLOT: u32 = 4;
}

impl Body {
    /// The opcode.
    pub fn opcode(&self) -> u16 {
        match self {
            Body::Campaign(_) => opcode::CAMPAIGN,
            Body::Enumerate => opcode::ENUMERATE,
            Body::Burst(_) => opcode::BURST,
            Body::Rates(_) => opcode::RATES,
            Body::GroupOption { .. } => opcode::GROUP_OPTION,
            Body::AirbaseOwner { .. } => opcode::AIRBASE_OWNER,
            Body::WarehouseSet { .. } => opcode::WAREHOUSE_SET,
            Body::WarehouseAdjust { .. } => opcode::WAREHOUSE_ADJUST,
            Body::Damage { .. } => opcode::DAMAGE,
            Body::Sound { .. } => opcode::SOUND,
            Body::Picture { .. } => opcode::PICTURE,
            Body::PictureStop => opcode::PICTURE_STOP,
            Body::GroupSpawn { .. } => opcode::GROUP_SPAWN,
            Body::GroupOrder(_) => opcode::GROUP_ORDER,
            Body::GroupDestroy => opcode::GROUP_DESTROY,
            Body::GroupCaptureDestroy => opcode::GROUP_CAPTURE_DESTROY,
            Body::StaticSpawn { .. } => opcode::STATIC_SPAWN,
            Body::StaticDestroy => opcode::STATIC_DESTROY,
            Body::ExtensionSet { .. } => opcode::EXTENSION_SET,
            Body::ExtensionRemove { .. } => opcode::EXTENSION_REMOVE,
            Body::MarkSet { .. } => opcode::MARK_SET,
            Body::MarkRemove => opcode::MARK_REMOVE,
            Body::DrawingSet { .. } => opcode::DRAWING_SET,
            Body::DrawingRemove => opcode::DRAWING_REMOVE,
            Body::MenuSet { .. } => opcode::MENU_SET,
            Body::MenuRemove => opcode::MENU_REMOVE,
            Body::Message { .. } => opcode::MESSAGE,
            Body::Chat { .. } => opcode::CHAT,
            Body::MissionLoad { .. } => opcode::MISSION_LOAD,
            Body::Eval { .. } => opcode::EVAL,
            Body::ExtensionInvoke { .. } => opcode::EXTENSION_INVOKE,
            Body::Capture => opcode::CAPTURE,
            Body::Countries => opcode::COUNTRIES,
            Body::Subscribe(_) => opcode::SUBSCRIBE,
            Body::Bind(_) => opcode::BIND,
            Body::Moderate { .. } => opcode::MODERATE,
            Body::Players => opcode::PLAYERS,
            Body::Bans => opcode::BANS,
        }
    }

    /// Whether the opcode is a control command: no sequence number, no epoch.
    pub fn is_control(&self) -> bool {
        matches!(
            self,
            Body::Campaign(_)
                | Body::Enumerate
                | Body::Burst(_)
                | Body::Rates(_)
                | Body::Subscribe(_)
                | Body::Bind(_)
        )
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
            Body::Rates(r) => {
                w.f64(r.position_s)
                    .f64(r.vitals_s)
                    .f64(r.ammo_s)
                    .f64(r.warehouse_s)
                    .f64(r.enumeration_s);
            }
            Body::Subscribe(sub) => {
                w.f64(sub.period_s)
                    .u32(sub.attitude)
                    .u32(sub.entity.len() as u32);
                for e in &sub.entity {
                    w.u64(*e);
                }
            }
            Body::Bind(b) => {
                let n = b.entity.len().min(b.object.len());
                w.u32(n as u32).u32(b.kind);
                for e in &b.entity[..n] {
                    w.u64(*e);
                }
                for o in &b.object[..n] {
                    w.u64(*o);
                }
            }
            Body::Moderate {
                action,
                player,
                side,
                seconds,
                ucid,
                slot,
                reason,
            } => {
                w.u32(*action)
                    .u32(*player)
                    .u32(*side)
                    .pad(4)
                    .f64(*seconds)
                    .str(ucid)
                    .str(slot)
                    .str(reason);
            }
            Body::GroupOption { option, value } => {
                w.u32(*option).pad(4).lv(value);
            }
            Body::AirbaseOwner { coalition, airbase } => {
                w.u32(*coalition).pad(4).str(airbase);
            }
            Body::WarehouseSet { airbase, stock: v }
            | Body::WarehouseAdjust { airbase, delta: v } => {
                w.str(airbase).lv(v);
            }
            Body::Damage { life_percent } => {
                w.f64(*life_percent);
            }
            Body::Sound {
                scope,
                target,
                file,
            } => {
                w.u32(*scope).pad(4).u64(*target).str(file);
            }
            Body::Picture {
                scope,
                target,
                clear_view,
                seconds,
                size,
                horizontal,
                vertical,
                size_units,
                file,
            } => {
                w.u32(*scope)
                    .u32(*clear_view)
                    .u64(*target)
                    .f64(*seconds)
                    .f64(*size)
                    .u32(*horizontal)
                    .u32(*vertical)
                    .u32(*size_units)
                    .pad(4)
                    .str(file);
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
            Body::ExtensionSet { name, source: v } | Body::ExtensionInvoke { name, args: v } => {
                w.str(name).lv(v);
            }
            Body::MarkSet {
                scope,
                read_only,
                target,
                x,
                y,
                z,
                text,
            } => {
                w.u32(*scope)
                    .u32(u32::from(*read_only))
                    .u64(*target)
                    .f64(*x)
                    .f64(*y)
                    .f64(*z)
                    .str(text);
            }
            Body::DrawingSet {
                scope,
                shape,
                target,
                args,
            } => {
                w.u32(*scope).u32(*shape).u64(*target).lv(args);
            }
            Body::MenuSet {
                parent,
                target,
                scope,
                submenu,
                text,
            } => {
                w.u64(*parent)
                    .u64(*target)
                    .u32(*scope)
                    .u32(u32::from(*submenu))
                    .str(text);
            }
            Body::ExtensionRemove { name } => {
                w.str(name);
            }
            Body::Message {
                scope,
                clear,
                target,
                seconds,
                text,
            } => {
                w.u32(*scope)
                    .u32(u32::from(*clear))
                    .u64(*target)
                    .f64(*seconds)
                    .str(text);
            }
            Body::Chat { player, text } => {
                w.u32(*player).pad(4).str(text);
            }
            Body::MissionLoad { path } => {
                w.str(path);
            }
            Body::Eval { source } => {
                w.lv(source);
            }
            Body::Enumerate
            | Body::GroupDestroy
            | Body::GroupCaptureDestroy
            | Body::StaticDestroy
            | Body::Players
            | Body::Bans
            | Body::MarkRemove
            | Body::DrawingRemove
            | Body::MenuRemove
            | Body::PictureStop
            | Body::Countries
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
            opcode::RATES => Body::Rates(Rates {
                position_s: r.f64()?,
                vitals_s: r.f64()?,
                ammo_s: r.f64()?,
                warehouse_s: r.f64()?,
                enumeration_s: r.f64()?,
            }),
            opcode::SUBSCRIBE => Body::Subscribe(Subscribe::decode(&payload[command::HEADER..])?),
            opcode::BIND => Body::Bind(Bind::decode(&payload[command::HEADER..])?),
            opcode::MODERATE => {
                let action = r.u32()?;
                let player = r.u32()?;
                let side = r.u32()?;
                r.skip(4)?;
                Body::Moderate {
                    action,
                    player,
                    side,
                    seconds: r.f64()?,
                    ucid: r.str()?.to_owned(),
                    slot: r.str()?.to_owned(),
                    reason: r.str()?.to_owned(),
                }
            }
            opcode::PLAYERS => Body::Players,
            opcode::BANS => Body::Bans,
            opcode::GROUP_OPTION => {
                let option = r.u32()?;
                r.skip(4)?;
                Body::GroupOption {
                    option,
                    value: r.lv()?,
                }
            }
            opcode::AIRBASE_OWNER => {
                let coalition = r.u32()?;
                r.skip(4)?;
                Body::AirbaseOwner {
                    coalition,
                    airbase: r.str()?.to_owned(),
                }
            }
            opcode::WAREHOUSE_SET => Body::WarehouseSet {
                airbase: r.str()?.to_owned(),
                stock: r.lv()?,
            },
            opcode::WAREHOUSE_ADJUST => Body::WarehouseAdjust {
                airbase: r.str()?.to_owned(),
                delta: r.lv()?,
            },
            opcode::DAMAGE => Body::Damage {
                life_percent: r.f64()?,
            },
            opcode::SOUND => {
                let scope = r.u32()?;
                r.skip(4)?;
                Body::Sound {
                    scope,
                    target: r.u64()?,
                    file: r.str()?.to_owned(),
                }
            }
            opcode::PICTURE => {
                let scope = r.u32()?;
                let clear_view = r.u32()?;
                let target = r.u64()?;
                let seconds = r.f64()?;
                let size = r.f64()?;
                let horizontal = r.u32()?;
                let vertical = r.u32()?;
                let size_units = r.u32()?;
                r.skip(4)?;
                Body::Picture {
                    scope,
                    target,
                    clear_view,
                    seconds,
                    size,
                    horizontal,
                    vertical,
                    size_units,
                    file: r.str()?.to_owned(),
                }
            }
            opcode::PICTURE_STOP => Body::PictureStop,
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
            opcode::EXTENSION_SET => Body::ExtensionSet {
                name: r.str()?.to_owned(),
                source: r.lv()?,
            },
            opcode::EXTENSION_INVOKE => Body::ExtensionInvoke {
                name: r.str()?.to_owned(),
                args: r.lv()?,
            },
            opcode::EXTENSION_REMOVE => Body::ExtensionRemove {
                name: r.str()?.to_owned(),
            },
            opcode::MARK_SET => {
                let scope = r.u32()?;
                let read_only = r.u32()? != 0;
                Body::MarkSet {
                    scope,
                    read_only,
                    target: r.u64()?,
                    x: r.f64()?,
                    y: r.f64()?,
                    z: r.f64()?,
                    text: r.str()?.to_owned(),
                }
            }
            opcode::MARK_REMOVE => Body::MarkRemove,
            opcode::DRAWING_SET => {
                let scope = r.u32()?;
                let shape = r.u32()?;
                Body::DrawingSet {
                    scope,
                    shape,
                    target: r.u64()?,
                    args: r.lv()?,
                }
            }
            opcode::DRAWING_REMOVE => Body::DrawingRemove,
            opcode::MENU_SET => {
                let parent = r.u64()?;
                let target = r.u64()?;
                let scope = r.u32()?;
                Body::MenuSet {
                    parent,
                    target,
                    scope,
                    submenu: r.u32()? != 0,
                    text: r.str()?.to_owned(),
                }
            }
            opcode::MENU_REMOVE => Body::MenuRemove,
            opcode::MESSAGE => {
                let scope = r.u32()?;
                let clear = r.u32()? != 0;
                Body::Message {
                    scope,
                    clear,
                    target: r.u64()?,
                    seconds: r.f64()?,
                    text: r.str()?.to_owned(),
                }
            }
            opcode::CHAT => {
                let player = r.u32()?;
                r.skip(4)?;
                Body::Chat {
                    player,
                    text: r.str()?.to_owned(),
                }
            }
            opcode::MISSION_LOAD => Body::MissionLoad {
                path: r.str()?.to_owned(),
            },
            opcode::EVAL => Body::Eval { source: r.lv()? },
            opcode::CAPTURE => Body::Capture,
            opcode::COUNTRIES => Body::Countries,
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
            ("ENUM_BEGIN", Enumeration::default().encode_begin().len()),
            ("ENUM_END", Enumeration::default().encode_end().len()),
            ("SEEN", Seen::default().encode().len()),
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
                name: "np.blp9".into(),
                type_name: "M-113".into(),
                group_name: "np!blp8".into(),
            }),
            Input::ObjectGone(ObjectGone {
                object: 7,
                reason: 3,
                entity: 9,
                group_entity: 8,
            }),
            Input::EnumBegin(Enumeration {
                enumeration: 4,
                objects: 0,
                seen: 0,
            }),
            Input::Seen(Seen {
                enumeration: 4,
                objects: vec![7, 8],
            }),
            Input::EnumEnd(Enumeration {
                enumeration: 4,
                objects: 11,
                seen: 2,
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
            Input::AuthorEvent(AuthorEvent {
                objects: vec![7],
                name: "tick".into(),
                payload: lv::obj(vec![("n", lv::n(1.0))]),
            }),
            Input::Player(Player {
                event: 4,
                player: 2,
                side: 1,
                seat: 0,
                client: "c".into(),
                name: "n".into(),
                slot: "s".into(),
                text: "hi".into(),
            }),
            Input::Menu(Menu {
                entity: 5,
                target: 6,
                scope: 3,
                player: 2,
            }),
            Input::Extension(ExtensionStats {
                calls: 3,
                total_us: 30,
                max_us: 20,
                rev: 2,
                events_skipped: 1,
                stopped: 0,
                name: "count".into(),
            }),
        ];
        for i in inputs {
            let (k, p) = i.encode();
            assert_eq!(Input::decode(k, &p).unwrap(), i);
        }
    }

    #[test]
    fn a_player_facing_record_that_is_cut_short_fails() {
        for (k, p) in [
            Input::Menu(Menu::default()).encode(),
            Input::Player(Player::default()).encode(),
            Input::Extension(ExtensionStats::default()).encode(),
            Input::AuthorEvent(AuthorEvent {
                objects: vec![1],
                name: String::new(),
                payload: Lv::Nil,
            })
            .encode(),
        ] {
            assert!(Input::decode(k, &p[..p.len() - 1]).is_err(), "kind {k}");
        }
    }

    #[test]
    fn short_and_long_row_counts_fail() {
        let mut p = encode_positions(&[Position::default()]);
        p[0] = 2;
        assert!(decode_positions(&p).is_err());
        assert!(decode_vitals(&[0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0]).is_err());
    }

    /// The byte layouts of spec/commands.md, pad fields included, and the
    /// fixed sizes of the golden file.
    #[test]
    fn new_commands_have_the_spec_s_layouts() {
        let head = command::HEADER;
        let body = |b: Body| {
            let c = Command {
                header: CommandHeader::default(),
                body: b,
            };
            let (_, p) = c.encode();
            p[head..].to_vec()
        };
        let o = body(Body::GroupOption {
            option: 7,
            value: Lv::Nil,
        });
        assert_eq!(&o[..4], &7u32.to_le_bytes());
        assert_eq!(&o[4..8], &[0; 4]);
        assert_eq!(o.len(), golden::layout("GROUP_OPTION").unwrap().fixed + 1);
        let a = body(Body::AirbaseOwner {
            coalition: 2,
            airbase: "X".into(),
        });
        assert_eq!(&a[..4], &2u32.to_le_bytes());
        assert_eq!(&a[4..8], &[0; 4]);
        assert_eq!(golden::layout("AIRBASE_OWNER").unwrap().fixed, 8);
        let d = body(Body::Damage { life_percent: 10.0 });
        assert_eq!(d, 10.0f64.to_le_bytes());
        assert_eq!(d.len(), golden::layout("DAMAGE").unwrap().fixed);
        let sound = body(Body::Sound {
            scope: 2,
            target: 1,
            file: "x".into(),
        });
        assert_eq!(&sound[..4], &2u32.to_le_bytes());
        assert_eq!(&sound[4..8], &[0; 4]);
        assert_eq!(&sound[8..16], &1u64.to_le_bytes());
        assert_eq!(sound.len(), golden::layout("SOUND").unwrap().fixed + 3);
        let pic = body(Body::Picture {
            scope: 1,
            target: 0,
            clear_view: 1,
            seconds: 5.0,
            size: 50.0,
            horizontal: 1,
            vertical: 2,
            size_units: 3,
            file: "p".into(),
        });
        assert_eq!(&pic[4..8], &1u32.to_le_bytes());
        assert_eq!(&pic[16..24], &5.0f64.to_le_bytes());
        assert_eq!(&pic[24..32], &50.0f64.to_le_bytes());
        assert_eq!(&pic[40..44], &3u32.to_le_bytes());
        assert_eq!(&pic[44..48], &[0; 4]);
        assert_eq!(pic.len(), golden::layout("PICTURE").unwrap().fixed + 3);
        assert!(body(Body::PictureStop).is_empty());
        let mark = body(Body::MarkSet {
            scope: 3,
            read_only: true,
            target: 9,
            x: 1.0,
            y: 2.0,
            z: 3.0,
            text: "t".into(),
        });
        assert_eq!(&mark[..4], &3u32.to_le_bytes());
        assert_eq!(&mark[4..8], &1u32.to_le_bytes());
        assert_eq!(&mark[8..16], &9u64.to_le_bytes());
        assert_eq!(&mark[16..24], &1.0f64.to_le_bytes());
        assert_eq!(&mark[24..32], &2.0f64.to_le_bytes());
        assert_eq!(&mark[32..40], &3.0f64.to_le_bytes());
        assert_eq!(mark.len(), golden::layout("MARK_SET").unwrap().fixed + 3);
        let drawing = body(Body::DrawingSet {
            scope: 2,
            shape: 7,
            target: 1,
            args: Lv::Nil,
        });
        assert_eq!(&drawing[..4], &2u32.to_le_bytes());
        assert_eq!(&drawing[4..8], &7u32.to_le_bytes());
        assert_eq!(&drawing[8..16], &1u64.to_le_bytes());
        assert_eq!(
            drawing.len(),
            golden::layout("DRAWING_SET").unwrap().fixed + 1
        );
        let menu = body(Body::MenuSet {
            parent: 5,
            target: 6,
            scope: 3,
            submenu: true,
            text: "m".into(),
        });
        assert_eq!(&menu[..8], &5u64.to_le_bytes());
        assert_eq!(&menu[8..16], &6u64.to_le_bytes());
        assert_eq!(&menu[16..20], &3u32.to_le_bytes());
        assert_eq!(&menu[20..24], &1u32.to_le_bytes());
        assert_eq!(menu.len(), golden::layout("MENU_SET").unwrap().fixed + 3);
        let msg = body(Body::Message {
            scope: 2,
            clear: true,
            target: 4,
            seconds: 7.5,
            text: "hi".into(),
        });
        assert_eq!(&msg[..4], &2u32.to_le_bytes());
        assert_eq!(&msg[4..8], &1u32.to_le_bytes());
        assert_eq!(&msg[8..16], &4u64.to_le_bytes());
        assert_eq!(&msg[16..24], &7.5f64.to_le_bytes());
        assert_eq!(msg.len(), golden::layout("MESSAGE").unwrap().fixed + 4);
        let chat = body(Body::Chat {
            player: 3,
            text: "x".into(),
        });
        assert_eq!(&chat[..4], &3u32.to_le_bytes());
        assert_eq!(&chat[4..8], &[0; 4]);
        assert_eq!(chat.len(), golden::layout("CHAT").unwrap().fixed + 3);
        for n in ["EXTENSION_REMOVE", "MISSION_LOAD", "EVAL"] {
            assert_eq!(golden::layout(n).unwrap().fixed, 0);
        }
        assert_eq!(
            body(Body::ExtensionRemove { name: "e".into() }),
            [1, 0, b'e']
        );
        assert_eq!(body(Body::MissionLoad { path: "p".into() }), [1, 0, b'p']);
        assert_eq!(
            body(Body::Eval { source: Lv::Nil }).len(),
            golden::layout("EVAL").unwrap().fixed + 1
        );
        for b in [Body::MarkRemove, Body::DrawingRemove, Body::MenuRemove] {
            assert!(body(b).is_empty());
        }
        let m = body(Body::Moderate {
            action: moderation::SLOT,
            player: 7,
            side: 2,
            seconds: 30.5,
            ucid: "u".into(),
            slot: "s".into(),
            reason: "r".into(),
        });
        assert_eq!(&m[..4], &4u32.to_le_bytes());
        assert_eq!(&m[4..8], &7u32.to_le_bytes());
        assert_eq!(&m[8..12], &2u32.to_le_bytes());
        assert_eq!(&m[12..16], &[0; 4]);
        assert_eq!(&m[16..24], &30.5f64.to_le_bytes());
        assert_eq!(m.len(), golden::layout("MODERATE").unwrap().fixed + 9);
        assert!(body(Body::Players).is_empty() && body(Body::Bans).is_empty());
        let sub = body(Body::Subscribe(Subscribe {
            period_s: 1.0,
            attitude: 0,
            entity: vec![4, 5],
        }));
        assert_eq!(sub.len(), golden::layout("SUBSCRIBE").unwrap().fixed + 16);
        let r = body(Body::Rates(Rates::default()));
        assert_eq!(r.len(), golden::layout("RATES").unwrap().fixed);
        for n in ["WAREHOUSE_SET", "WAREHOUSE_ADJUST"] {
            assert_eq!(golden::layout(n).unwrap().fixed, 0);
        }
        assert!(
            Command {
                header: CommandHeader::default(),
                body: Body::Rates(Rates::default())
            }
            .body
            .is_control()
        );
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
                name: "np!vvY4".into(),
                group: lv::obj(vec![("name", lv::s("np!vvY4"))]),
            },
            Body::GroupOrder(lv::obj(vec![("id", lv::s("Hold"))])),
            Body::GroupDestroy,
            Body::GroupCaptureDestroy,
            Body::StaticSpawn {
                country: 2,
                name: "np#vvY4".into(),
                table: lv::obj(vec![("type", lv::s("Tent"))]),
            },
            Body::StaticDestroy,
            Body::ExtensionSet {
                name: "count".into(),
                source: lv::s("return {}"),
            },
            Body::ExtensionInvoke {
                name: "count".into(),
                args: lv::obj(vec![("1", lv::s("a"))]),
            },
            Body::Capture,
            Body::Countries,
            Body::Rates(Rates {
                position_s: 1.0,
                vitals_s: 2.0,
                ammo_s: 3.0,
                warehouse_s: 4.0,
                enumeration_s: 5.0,
            }),
            Body::GroupOption {
                option: 3,
                value: lv::n(2.0),
            },
            Body::AirbaseOwner {
                coalition: 2,
                airbase: "Batumi".into(),
            },
            Body::WarehouseSet {
                airbase: "Batumi".into(),
                stock: lv::obj(vec![("weapon", lv::obj(vec![("AIM-9X", lv::n(4.0))]))]),
            },
            Body::WarehouseAdjust {
                airbase: "Batumi".into(),
                delta: lv::obj(vec![("liquids", lv::list(vec![lv::n(-5.0)]))]),
            },
            Body::Damage { life_percent: 12.5 },
            Body::Sound {
                scope: 5,
                target: 80,
                file: "gong.ogg".into(),
            },
            Body::Picture {
                scope: 3,
                target: 1 << 40,
                clear_view: 1,
                seconds: 12.5,
                size: 60.0,
                horizontal: 1,
                vertical: 2,
                size_units: 0,
                file: "map.png".into(),
            },
            Body::PictureStop,
            Body::MarkSet {
                scope: 3,
                read_only: true,
                target: 1 << 40,
                x: 1.5,
                y: 2.5,
                z: -3.5,
                text: "rally".into(),
            },
            Body::MarkRemove,
            Body::DrawingSet {
                scope: 2,
                shape: 7,
                target: 1,
                args: lv::list(vec![lv::obj(vec![("x", lv::n(1.0)), ("z", lv::n(2.0))])]),
            },
            Body::DrawingRemove,
            Body::MenuSet {
                parent: 12,
                target: 2,
                scope: 2,
                submenu: false,
                text: "Go".into(),
            },
            Body::MenuRemove,
            Body::ExtensionRemove {
                name: "count".into(),
            },
            Body::Message {
                scope: 5,
                clear: false,
                target: 80,
                seconds: 10.0,
                text: "hello".into(),
            },
            Body::Chat {
                player: 4,
                text: "hi".into(),
            },
            Body::MissionLoad {
                path: "missions/a.miz".into(),
            },
            Body::Eval {
                source: lv::s("return 1"),
            },
            Body::Moderate {
                action: moderation::BAN,
                player: 0,
                side: 0,
                seconds: 3600.0,
                ucid: "0123".into(),
                slot: String::new(),
                reason: "spam".into(),
            },
            Body::Players,
            Body::Bans,
            Body::Subscribe(Subscribe {
                period_s: 2.0,
                attitude: 1,
                entity: vec![1, 9],
            }),
            Body::Bind(Bind {
                kind: 3,
                entity: vec![5, 6],
                object: vec![70, 80],
            }),
        ] {
            let c = Command { header: h, body };
            let (op, p) = c.encode();
            assert_eq!(Command::decode(op, &p).unwrap(), Some(c));
        }
        assert!(Command::decode(16, &[0; 10]).is_err());
        assert_eq!(Command::decode(48, &[0; 48]).unwrap(), None);
    }
}
