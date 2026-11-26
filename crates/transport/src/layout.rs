//! Offsets and codes from the transport specification.
//!
//! Each constant is checked against `spec/transport.golden` in the tests below,
//! and each table of the golden file this crate uses is checked to be complete.

/// The status block: offsets of its fields, from the start of the file.
pub mod status {
    /// Bytes in the status block.
    pub const SIZE: usize = 4096;

    /// ASCII `npmap` and three zero bytes, read as a little-endian `u64`.
    pub const MAGIC_VALUE: u64 = u64::from_le_bytes(*b"npmap\0\0\0");
    /// The `layout_version` this crate writes and accepts.
    pub const LAYOUT_VERSION_VALUE: u32 = 2;

    macro_rules! fields {
        ($($konst:ident $name:literal $off:literal $width:literal $count:literal $writer:ident;)*) => {
            $(
                #[doc = concat!("`", $name, "`")]
                pub const $konst: usize = $off;
            )*
            /// Every field that is not padding: name, offset, bytes of one
            /// value, values, writer.
            pub const FIELDS: &[(&str, usize, usize, usize, super::Writer)] = &[
                $(($name, $off, $width, $count, super::Writer::$writer),)*
            ];
        };
    }

    fields! {
        MAGIC "magic" 0x0000 8 1 Agent;
        LAYOUT_VERSION "layout_version" 0x0008 4 1 Agent;
        INTERFACE "interface" 0x000C 4 1 Agent;
        LAYOUT_HASH "layout_hash" 0x0010 8 1 Agent;
        FILE_SIZE "file_size" 0x0018 8 1 Agent;
        AGENT_BOOT "agent_boot" 0x0020 8 1 Agent;
        BUILD "build" 0x0028 8 1 Agent;
        CREATED_MS "created_ms" 0x0030 8 1 Agent;
        CLOCK_HZ "clock_hz" 0x0038 8 1 Agent;
        OUT_OFFSET "out_offset" 0x0040 8 1 Agent;
        OUT_CAPACITY "out_capacity" 0x0048 8 1 Agent;
        IN_OFFSET "in_offset" 0x0050 8 1 Agent;
        IN_CAPACITY "in_capacity" 0x0058 8 1 Agent;
        READY "ready" 0x0060 8 1 Agent;
        FRAME "frame" 0x0080 8 1 Agent;
        CLOCK "clock" 0x0088 8 1 Agent;
        MODEL_TIME "model_time" 0x0090 8 1 Agent;
        REAL_TIME "real_time" 0x0098 8 1 Agent;
        LIFECYCLE "lifecycle" 0x00A0 4 1 Agent;
        MISSION_GEN "mission_gen" 0x00A4 4 1 Agent;
        CAMPAIGN "campaign" 0x00A8 8 1 Agent;
        OUT_TAIL "out_tail" 0x00B0 8 1 Agent;
        IN_HEAD "in_head" 0x00B8 8 1 Agent;
        EPOCH "epoch" 0x00C0 8 1 Agent;
        LAST_APPLIED_SEQ "last_applied_seq" 0x00C8 8 1 Agent;
        SIDECAR_BOOT_SEEN "sidecar_boot_seen" 0x00D0 8 1 Agent;
        FLAGS "flags" 0x00D8 4 1 Agent;
        CONTROL_QUEUED "control_queued" 0x00DC 4 1 Agent;
        CRUMB "crumb" 0x00E0 8 1 Agent;
        CRUMB_ARG "crumb_arg" 0x00E8 8 1 Agent;
        CRUMB_FRAME "crumb_frame" 0x00F0 8 1 Agent;
        CRUMB_CLOCK "crumb_clock" 0x00F8 8 1 Agent;
        SAMPLES_DROPPED "samples_dropped" 0x0100 8 1 Agent;
        EVENTS_DROPPED "events_dropped" 0x0108 8 1 Agent;
        LOGS_DROPPED "logs_dropped" 0x0110 8 1 Agent;
        GAPS "gaps" 0x0118 8 1 Agent;
        COMMANDS_APPLIED "commands_applied" 0x0120 8 1 Agent;
        COMMANDS_OVER_BUDGET "commands_over_budget" 0x0128 8 1 Agent;
        LUA_HEAP_KIB "lua_heap_kib" 0x0130 8 1 Agent;
        EVENT_QUEUE_BYTES "event_queue_bytes" 0x0138 8 1 Agent;
        SIDECAR_BOOT "sidecar_boot" 0x0180 8 1 Sidecar;
        SIDECAR_HEARTBEAT "sidecar_heartbeat" 0x0188 8 1 Sidecar;
        AGENT_BOOT_SEEN "agent_boot_seen" 0x0190 8 1 Sidecar;
        IN_TAIL "in_tail" 0x0198 8 1 Sidecar;
        OUT_HEAD "out_head" 0x01A0 8 1 Sidecar;
        SIDECAR_MS "sidecar_ms" 0x01A8 8 1 Sidecar;
        ATTACH_SEEN "attach_seen" 0x01B0 8 1 Sidecar;
        METRICS_SEQ "metrics_seq" 0x0200 8 1 Agent;
        SLICE_US "slice_us" 0x0208 8 1 Agent;
        FRAME_INTERVAL_US "frame_interval_us" 0x0210 8 1 Agent;
        PERIOD_POSITION "period_position" 0x0218 8 1 Agent;
        PERIOD_VITALS "period_vitals" 0x0220 8 1 Agent;
        PERIOD_WAREHOUSE "period_warehouse" 0x0230 8 1 Agent;
        PERIOD_ENUMERATION "period_enumeration" 0x0238 8 1 Agent;
        ROWS_PER_CLOCK "rows_per_clock" 0x0240 8 1 Agent;
        HIST_PUMP "hist_pump" 0x0280 8 32 Agent;
        HIST_INTERVAL "hist_interval" 0x0380 8 32 Agent;
        HIST_COMMAND "hist_command" 0x0480 8 32 Agent;
        HIST_EVENT "hist_event" 0x0580 8 32 Agent;
        HIST_SYSCALL "hist_syscall" 0x0680 8 32 Agent;
    }

    /// The first agent field after the header line: what Start zeros.
    pub const AGENT_FIELDS: core::ops::Range<usize> = FRAME..SIDECAR_BOOT;
    /// The sidecar's line.
    pub const SIDECAR_FIELDS: core::ops::Range<usize> = SIDECAR_BOOT..METRICS_SEQ;
    /// The metrics fields under the sequence lock: `slice_us` to the end of
    /// `hist_syscall`.
    pub const METRICS: core::ops::Range<usize> = SLICE_US..HIST_SYSCALL + 32 * 8;
    /// Buckets in each histogram.
    pub const HIST_BUCKETS: usize = 32;
    /// The agent's `pad` fields: offset and bytes.
    pub const AGENT_PADS: &[(usize, usize)] = &[
        (0x68, 24),
        (0x140, 64),
        (0x228, 8),
        (0x248, 56),
        (0x780, 2176),
    ];
}

/// The side that writes a status field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Writer {
    /// The agent, inside DCS.
    Agent,
    /// The sidecar.
    Sidecar,
}

/// The record header.
pub mod record {
    /// Bytes in the header.
    pub const HEADER: usize = 8;
    /// `len`: payload bytes after the header, not counting padding.
    pub const LEN: usize = 0;
    /// `kind`: a record kind or an opcode.
    pub const KIND: usize = 4;
    /// `flags`: 0.
    pub const FLAGS: usize = 6;
}

/// The command header at the start of every inbound payload.
pub mod command {
    /// Bytes in the header.
    pub const HEADER: usize = 48;
    /// `seq`
    pub const SEQ: usize = 0x00;
    /// `epoch`
    pub const EPOCH: usize = 0x08;
    /// `campaign`
    pub const CAMPAIGN: usize = 0x10;
    /// `entity`
    pub const ENTITY: usize = 0x18;
    /// `rev`
    pub const REV: usize = 0x20;
    /// `mission_gen`
    pub const MISSION_GEN: usize = 0x28;
}

macro_rules! codes {
    ($(#[$doc:meta])* $module:ident $section:literal { $($konst:ident = $value:literal,)* }) => {
        $(#[$doc])*
        pub mod $module {
            $(
                #[doc = concat!("`", stringify!($konst), "`")]
                pub const $konst: u64 = $value;
            )*
            /// The golden file's section these codes come from.
            pub const SECTION: &str = $section;
            /// Every code in the section, by name.
            pub const ALL: &[(&str, u64)] = &[$((stringify!($konst), $value),)*];
        }
    };
}

codes! {
    /// `lifecycle` codes.
    lifecycle "lifecycle" {
        STARTING = 1, LOADING = 2, RUNNING = 3, PAUSED = 4, STOPPING = 5, DISABLED = 6,
    }
}

codes! {
    /// `agent_flags` bits.
    flags "agent_flags" {
        ATTACHED = 1, CONTROL_LOST = 2, COMMANDS_HELD = 4, RAW_LUA = 8, NO_HANDSHAKE = 16,
    }
}

codes! {
    /// Outbound record kinds.
    kind "record_kind" {
        PADDING = 0, FRAME = 1, HELLO = 2, LIFECYCLE = 3, RESULT = 4, GAP = 5,
        OBJECT = 16, OBJECT_GONE = 17, ENUM_BEGIN = 18, ENUM_END = 19, EVENT = 20,
        AUTHOR_EVENT = 21, PLAYER = 22, MENU = 23, LOG = 24, SEEN = 25,
        POSITIONS = 32, VITALS = 33, ATTITUDE = 34, AMMO = 35, WAREHOUSE = 36,
        AIRBASE = 37, EXTENSION = 38,
    }
}

codes! {
    /// `gap_class` codes.
    gap_class "gap_class" { EVENTS = 1, LOGS = 2, }
}

codes! {
    /// `lv_tag` codes.
    lv_tag "lv_tag" { NIL = 0, FALSE = 1, TRUE = 2, NUMBER = 3, STRING = 4, TABLE = 5, OTHER = 6, }
}

codes! {
    /// Command results.
    result "result" {
        APPLIED = 0, AT_REVISION = 2, DUPLICATE = 3, STALE_EPOCH = 16, NO_CAMPAIGN = 17,
        NOT_BOUND = 18, BAD_PAYLOAD = 19, UNKNOWN_OPCODE = 20, RAW_LUA_OFF = 21,
        DCS_REFUSED = 22, LUA_ERROR = 23, WRONG_CAMPAIGN = 24, TOO_LARGE = 25,
        WRONG_GENERATION = 26, NOT_READY = 27,
    }
}

codes! {
    /// Command opcodes, the `kind` of an inbound record (spec/commands.md).
    opcode "opcode" {
        PADDING = 0, CAMPAIGN = 1, ENUMERATE = 2, RATES = 3, SUBSCRIBE = 4, BIND = 5,
        BURST = 6, GROUP_SPAWN = 16, GROUP_ORDER = 17, GROUP_DESTROY = 18,
        GROUP_CAPTURE_DESTROY = 19, GROUP_OPTION = 20, STATIC_SPAWN = 21,
        STATIC_DESTROY = 22, AIRBASE_OWNER = 23, WAREHOUSE_SET = 24, MARK_SET = 25,
        MARK_REMOVE = 26, DRAWING_SET = 27, DRAWING_REMOVE = 28, MENU_SET = 29,
        MENU_REMOVE = 30, EXTENSION_SET = 31, EXTENSION_REMOVE = 32, DRAWING_UPDATE = 33,
        EXPLOSION = 48,
        MESSAGE = 49, SOUND = 50, SMOKE = 51, FLARE = 52, ILLUMINATION = 53, DAMAGE = 54,
        CHAT = 55, MISSION_LOAD = 56, EXTENSION_INVOKE = 57, EVAL = 58,
        WAREHOUSE_ADJUST = 59, PICTURE = 60, PICTURE_STOP = 61, MODERATE = 62,
        CLEAR_DEBRIS = 63, CAPTURE = 64, COUNTRIES = 65, PLAYERS = 66, BANS = 67,
        MARK_CLEAR = 68,
    }
}

codes! {
    /// `MODERATE` actions (spec/commands.md, "Codes: moderation").
    moderation "moderation" { KICK = 1, BAN = 2, UNBAN = 3, SLOT = 4, }
}

/// `MODERATE SLOT`'s side for "the side the agent resolves": the player's
/// own, or the slot's coalition (spec/commands.md, "Slot moves").
pub const SLOT_KEEP: u32 = 0xFFFF_FFFF;

codes! {
    /// `CLEAR_DEBRIS` volume shapes (spec/commands.md, "Codes: volume").
    volume "volume" { SPHERE = 1, BOX = 2, }
}

/// The delivery class of a command (spec/commands.md, "Delivery classes").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandClass {
    /// In order, not sequenced.
    Control,
    /// At least once, de-duplicated.
    SetState,
    /// At most once.
    Effect,
    /// At least once; changes nothing.
    Query,
}

/// The revision a command carries: the opcode table's Rev column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rev {
    /// Compared with the applied spec revision of the entity, or of the
    /// named extension for `EXTENSION_SET`.
    Spec,
    /// Compared with the applied order revision of the entity.
    Order,
}

/// One row of the opcode table (spec/commands.md, "Codes: opcode").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opcode {
    /// The delivery class.
    pub class: CommandClass,
    /// The revision it carries, if any.
    pub rev: Option<Rev>,
    /// The Campaign column is `active`: it needs an active campaign.
    pub needs_campaign: bool,
    /// The Gen column is `bound`: it is refused in another generation.
    pub gen_bound: bool,
    /// The State column: the Lua state its handler runs in, `None` for a
    /// control command.
    pub state: Option<HandlerState>,
}

/// The Lua state a command's handler runs in (spec/commands.md, "Handlers").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandlerState {
    /// The hook's own state.
    Hook,
    /// The mission scripting state, through the shim.
    Server,
}

/// The row of an opcode, `None` for padding or an unknown opcode.
pub const fn opcode_row(code: u64) -> Option<Opcode> {
    use CommandClass::*;
    const fn row(
        class: CommandClass,
        rev: Option<Rev>,
        needs_campaign: bool,
        gen_bound: bool,
    ) -> Option<Opcode> {
        let state = match class {
            Control => None,
            _ => Some(HandlerState::Server),
        };
        Some(Opcode {
            class,
            rev,
            needs_campaign,
            gen_bound,
            state,
        })
    }
    match code {
        1..=6 => row(Control, None, false, false),
        16 | 21 | 25 | 27 | 29 | 33 => row(SetState, Some(Rev::Spec), true, true),
        17 => row(SetState, Some(Rev::Order), true, true),
        18..=20 | 22..=24 | 26 | 28 | 30 => row(SetState, None, true, true),
        31 => row(SetState, Some(Rev::Spec), true, false),
        32 => row(SetState, None, true, false),
        48 | 51..=53 | 59 => row(Effect, None, true, true),
        54 => Some(Opcode {
            class: Effect,
            rev: None,
            needs_campaign: true,
            gen_bound: true,
            state: Some(HandlerState::Hook),
        }),
        49 | 50 => row(Effect, None, false, false),
        55 | 56 | 60 | 61 | 62 => Some(Opcode {
            state: Some(HandlerState::Hook),
            ..Opcode {
                class: Effect,
                rev: None,
                needs_campaign: false,
                gen_bound: false,
                state: None,
            }
        }),
        57 | 58 => row(Effect, None, true, false),
        63 | 68 => row(Effect, None, false, true),
        64 => row(Query, None, true, true),
        65 => row(Query, None, false, true),
        66 | 67 => Some(Opcode {
            state: Some(HandlerState::Hook),
            ..Opcode {
                class: Query,
                rev: None,
                needs_campaign: false,
                gen_bound: false,
                state: None,
            }
        }),
        _ => None,
    }
}

/// The delivery class of a record kind or an opcode, from the golden file's
/// `class` column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Never dropped.
    Control,
    /// Queued; dropped oldest first with a `GAP`.
    Event,
    /// Dropped and counted.
    Sample,
}

/// The class of an outbound record kind, `None` for padding or an unknown kind.
pub fn record_class(kind: u64) -> Option<Class> {
    match kind {
        1..=5 => Some(Class::Control),
        16..=25 => Some(Class::Event),
        32..=38 => Some(Class::Sample),
        _ => None,
    }
}

/// Ring capacities the file may have: powers of two from 256 KiB to 64 MiB.
pub const CAPACITY: core::ops::RangeInclusive<u64> = (256 << 10)..=(64 << 20);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::golden;

    #[test]
    fn the_status_block_matches_the_golden_file() {
        let layout = golden::layout("status").expect("status");
        assert_eq!(layout.fixed, status::SIZE);
        let real: Vec<_> = layout
            .fields
            .iter()
            .filter(|f| f.element().0 != "pad")
            .collect();
        assert_eq!(real.len(), status::FIELDS.len());
        for (f, &(name, off, width, count, writer)) in real.iter().zip(status::FIELDS) {
            assert_eq!(f.name, name);
            assert_eq!(f.offset, off, "{name}");
            let (ty, n) = f.element();
            assert_eq!(golden::width(ty), Some(width), "{name}");
            assert_eq!(n, count, "{name}");
            let w = match writer {
                Writer::Agent => "agent",
                Writer::Sidecar => "sidecar",
            };
            assert_eq!(f.writer, Some(w), "{name}");
        }
    }

    #[test]
    fn the_agent_pads_match_the_golden_file() {
        let layout = golden::layout("status").expect("status");
        let pads: Vec<_> = layout
            .fields
            .iter()
            .filter(|f| f.element().0 == "pad" && f.writer == Some("agent"))
            .map(|f| (f.offset, f.size()))
            .collect();
        assert_eq!(pads, status::AGENT_PADS);
    }

    #[test]
    fn the_ranges_cover_whole_lines() {
        assert_eq!(status::AGENT_FIELDS, 0x80..0x180);
        assert_eq!(status::SIDECAR_FIELDS, 0x180..0x200);
        let hist = golden::layout("status").expect("status");
        let after = hist.field("reserved4").expect("reserved4").offset;
        assert_eq!(status::METRICS.end, after);
    }

    #[test]
    fn the_headers_match_the_golden_file() {
        let r = golden::layout("record").expect("record");
        assert_eq!(r.fixed, record::HEADER);
        for (name, off) in [
            ("len", record::LEN),
            ("kind", record::KIND),
            ("flags", record::FLAGS),
        ] {
            assert_eq!(r.field(name).map(|f| f.offset), Some(off), "{name}");
        }
        let c = golden::layout("command").expect("command");
        assert_eq!(c.fixed, command::HEADER);
        for (name, off) in [
            ("seq", command::SEQ),
            ("epoch", command::EPOCH),
            ("campaign", command::CAMPAIGN),
            ("entity", command::ENTITY),
            ("rev", command::REV),
            ("mission_gen", command::MISSION_GEN),
        ] {
            assert_eq!(c.field(name).map(|f| f.offset), Some(off), "{name}");
        }
    }

    fn same_codes(section: &str, all: &[(&str, u64)]) {
        let golden: Vec<_> = golden::codes(section)
            .into_iter()
            .map(|c| (c.name, c.code))
            .collect();
        assert_eq!(golden, all, "{section}");
    }

    #[test]
    fn the_codes_match_the_golden_file() {
        same_codes(lifecycle::SECTION, lifecycle::ALL);
        same_codes(flags::SECTION, flags::ALL);
        same_codes(kind::SECTION, kind::ALL);
        same_codes(gap_class::SECTION, gap_class::ALL);
        same_codes(lv_tag::SECTION, lv_tag::ALL);
        same_codes(result::SECTION, result::ALL);
        same_codes(opcode::SECTION, opcode::ALL);
        same_codes(moderation::SECTION, moderation::ALL);
        same_codes(volume::SECTION, volume::ALL);
    }

    #[test]
    fn the_layout_version_matches_the_golden_file() {
        assert_eq!(golden::layout_version(), Some(status::LAYOUT_VERSION_VALUE));
    }

    #[test]
    fn the_opcode_rows_match_the_golden_file() {
        for c in golden::codes("opcode") {
            let Some(row) = opcode_row(c.code) else {
                assert_eq!(c.code, opcode::PADDING, "{}", c.name);
                continue;
            };
            let class = match c.attr("class") {
                Some("control") => CommandClass::Control,
                Some("set-state") => CommandClass::SetState,
                Some("effect") => CommandClass::Effect,
                Some("query") => CommandClass::Query,
                other => panic!("{}: class {other:?}", c.name),
            };
            let rev = match c.attr("rev") {
                None => None,
                Some("spec") => Some(Rev::Spec),
                Some("order") => Some(Rev::Order),
                Some(other) => panic!("{}: rev {other}", c.name),
            };
            assert_eq!(row.class, class, "{}", c.name);
            assert_eq!(row.rev, rev, "{}", c.name);
            assert_eq!(
                row.needs_campaign,
                c.attr("campaign") == Some("active"),
                "{}",
                c.name
            );
            assert_eq!(row.gen_bound, c.attr("gen") == Some("bound"), "{}", c.name);
            let state = match c.attr("state") {
                None => None,
                Some("hook") => Some(HandlerState::Hook),
                Some("server") => Some(HandlerState::Server),
                Some(other) => panic!("{}: state {other}", c.name),
            };
            assert_eq!(row.state, state, "{}", c.name);
        }
        let golden: Vec<u64> = golden::codes("opcode").iter().map(|c| c.code).collect();
        for code in 0..=u64::from(u16::MAX) {
            if opcode_row(code).is_some() {
                assert!(golden.contains(&code), "{code} is not in the golden file");
            }
        }
    }

    #[test]
    fn the_record_classes_match_the_golden_file() {
        for c in golden::codes("record_kind") {
            let want = match c.attr("class") {
                Some("control") => Some(Class::Control),
                Some("event") => Some(Class::Event),
                Some("sample") => Some(Class::Sample),
                _ => None,
            };
            assert_eq!(record_class(c.code), want, "{}", c.name);
        }
    }
}
