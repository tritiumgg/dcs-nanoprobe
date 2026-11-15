//! The functions on the table `luaopen_nanoprobe` returns, and the one shape
//! every one of them has.
//!
//! An entry is the `extern "C"` function Lua calls. It owns nothing: it calls
//! [`guarded`], which runs the worker under `catch_unwind` in its own frame
//! and returns a plain integer. A worker that wants to raise a Lua error
//! leaves the message on the stack and returns [`RAISE`]; the entry then calls
//! `lua_error` holding nothing, so the `longjmp` crosses no Rust frame that
//! owns a value. `tools/raiseshape.sh` checks this in the built DLL.
//!
//! Workers never call a function that can raise, except where `lua_pcall`
//! protects the call.

use core::ffi::{CStr, c_int};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::Ordering;

use nanoprobe_transport::codec::Lv;
use nanoprobe_transport::layout::result;
use nanoprobe_transport::log::{Level, Value};
use nanoprobe_transport::map::MappedFile;
use nanoprobe_transport::mem::Memory;
use nanoprobe_transport::records::{ExtensionStats, Menu, Player};

use crate::lua::{self, State};
use crate::{driver, platform, state};

/// What a worker returns to ask its entry to raise the value on top of the
/// stack.
const RAISE: c_int = -1;

/// Run `work` for an entry: count the call, catch a panic, and honour the
/// disabled state unless `always`. An entry that is not `hot` runs outside
/// the frame's pump, so the agent's held log records are written after it;
/// a hot one leaves them for the pump's system-call frame (spec/log.md).
/// Returns the number of results, or [`RAISE`].
#[inline(never)]
unsafe fn guarded(
    state: *mut State,
    name: &'static str,
    always: bool,
    hot: bool,
    work: unsafe fn(*mut State) -> c_int,
) -> c_int {
    if !always && state::disabled() {
        return 0;
    }
    state::enter();
    // SAFETY: the worker's contract is the entry's: a live state from Lua.
    let result = std::panic::catch_unwind(|| unsafe { work(state) });
    state::leave();
    match result {
        Ok(n) => {
            if !hot {
                state::flush_log();
            }
            n
        }
        Err(_) => {
            state::trip(name);
            0
        }
    }
}

/// Define the entries: one `extern "C"` function per worker, and the list
/// the opener registers.
macro_rules! entries {
    ($($name:ident => $work:ident $(, $opt:ident)*;)*) => {
        $(
            /// An entry point; see the module documentation.
            ///
            /// # Safety
            ///
            /// Called by Lua with a live state.
            unsafe extern "C" fn $name(state: *mut State) -> c_int {
                // SAFETY: Lua passes a live state.
                let n = unsafe {
                    guarded(
                        state,
                        stringify!($name),
                        entries!(@always $($opt)*),
                        entries!(@hot $($opt)*),
                        $work,
                    )
                };
                if n == RAISE {
                    // SAFETY: the worker left the error value on top. This
                    // frame owns nothing.
                    unsafe { lua::lua_error(state) }
                } else {
                    n
                }
            }
        )*

        /// Every entry, by the name Lua sees.
        pub const ENTRIES: &[(&CStr, lua::CFunction)] = &[
            $(
                (
                    // SAFETY: a literal with one NUL at its end.
                    unsafe { CStr::from_bytes_with_nul_unchecked(concat!(stringify!($name), "\0").as_bytes()) },
                    $name as lua::CFunction,
                ),
            )*
        ];
    };
    (@always) => { false };
    (@always always $($rest:ident)*) => { true };
    (@always $other:ident $($rest:ident)*) => { entries!(@always $($rest)*) };
    (@hot) => { false };
    (@hot hot $($rest:ident)*) => { true };
    (@hot $other:ident $($rest:ident)*) => { entries!(@hot $($rest)*) };
}

entries! {
    status => work_status, always;
    configure => work_configure;
    start => work_start;
    frame => work_frame, hot;
    finish => work_finish, hot;
    command => work_command, hot;
    command_done => work_command_done, hot;
    deaths => work_deaths;
    reads => work_reads;
    spot => work_spot, hot;
    pump_begin => work_pump_begin, hot;
    plan => work_plan, hot;
    pump_end => work_pump_end, hot;
    pos => work_pos, hot;
    att => work_att, hot;
    stopped => work_stopped, hot;
    vit => work_vit, hot;
    ammo => work_ammo, hot;
    airbase => work_airbase, hot;
    item => work_item, hot;
    seen => work_seen, hot;
    object => work_object, hot;
    wants => work_wants, hot;
    enum_done => work_enum_done, hot;
    suspect => work_suspect, hot;
    checks => work_checks, hot;
    gone => work_gone, hot;
    alive => work_alive, hot;
    lifecycle => work_lifecycle;
    log => work_log, hot;
    event => work_event, hot;
    player => work_player;
    menu => work_menu, hot;
    emit => work_emit, hot;
    extension => work_extension, hot;
    extension_budget => work_extension_budget, hot;
    thread => work_thread, hot;
    clock => work_clock, hot;
    qpc_cost => work_qpc_cost;
    crumb => work_crumb, hot;
    raise => work_raise;
    call => work_call;
    panic => work_panic;
    fault => work_fault;
    sink => work_sink, hot;
    sink_take => work_sink_take, hot;
    csweep => work_csweep, hot;
    sock_open => work_sock_open;
    sock_frame => work_sock_frame, hot;
    sock_close => work_sock_close;
    sys_open => work_sys_open;
    sys_frame => work_sys_frame, hot;
    sys_close => work_sys_close;
    map_open => work_map_open;
    map_frame => work_map_frame, hot;
    map_load => work_map_load, hot;
    map_close => work_map_close;
    inject => work_inject;
}

/// Push `key = value` into the table on top of the stack.
unsafe fn field_str(state: *mut State, key: &CStr, value: &str) {
    // SAFETY: the caller has a table on top and a free slot.
    unsafe {
        lua::push_str(state, value);
        lua::lua_setfield(state, -2, key.as_ptr());
    }
}

unsafe fn field_num(state: *mut State, key: &CStr, value: f64) {
    // SAFETY: as for `field_str`.
    unsafe {
        lua::lua_pushnumber(state, value);
        lua::lua_setfield(state, -2, key.as_ptr());
    }
}

unsafe fn field_bool(state: *mut State, key: &CStr, value: bool) {
    // SAFETY: as for `field_str`.
    unsafe {
        lua::lua_pushboolean(state, c_int::from(value));
        lua::lua_setfield(state, -2, key.as_ptr());
    }
}

/// Leave `message` on the stack and ask the entry to raise it.
unsafe fn raise_with(state: *mut State, message: &str) -> c_int {
    // SAFETY: a live state with a free slot.
    unsafe { lua::push_str(state, message) };
    RAISE
}

/// Push nil and an error message: the Lua convention for a failed call.
unsafe fn fail(state: *mut State, message: &str) -> c_int {
    // SAFETY: a live state with two free slots.
    unsafe {
        lua::lua_pushnil(state);
        lua::push_str(state, message);
    }
    2
}

/// The argument at `index` as a number, or `default` when it is absent or
/// not a number.
unsafe fn arg_num(state: *mut State, index: c_int, default: f64) -> f64 {
    // SAFETY: a live state; `lua_type` accepts any index.
    if unsafe { lua::lua_type(state, index) } == lua::TNUMBER {
        // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
        unsafe { lua::lua_tonumber(state, index) }
    } else {
        default
    }
}

/// `status()`: a table describing the agent. Works while disabled.
unsafe fn work_status(state: *mut State) -> c_int {
    let root = state::root().map(|r| r.display().to_string());
    let link = driver::summary();
    // SAFETY: a live state; the table takes one slot, each field one more.
    unsafe {
        lua::lua_createtable(state, 0, 20);
        field_bool(state, c"disabled", state::disabled());
        field_str(state, c"reason", &state::reason());
        field_str(state, c"build", state::build());
        field_str(state, c"module", &platform::module_name());
        field_str(state, c"pdb", &platform::pdb_id());
        field_bool(state, c"pinned", state::pinned());
        field_num(state, c"thread", platform::thread_id() as f64);
        field_num(state, c"opens", crate::opens() as f64);
        field_num(state, c"crumb_code", state::crumb_code() as f64);
        if let Some(root) = &root {
            field_str(state, c"root", root);
        }
        field_bool(state, c"started", link.is_some());
        if let Some(l) = &link {
            field_str(state, c"map", &l.map);
            field_str(state, c"boot", &l.boot);
            field_bool(state, c"attached", l.attached);
            field_num(state, c"lifecycle", f64::from(l.lifecycle));
            field_num(state, c"gen", f64::from(l.mission_gen));
            field_num(state, c"flags", f64::from(l.flags));
            field_num(state, c"frame", l.frame as f64);
            field_num(state, c"pending_destroy", l.pending_destroy as f64);
            field_num(state, c"pending_overflow", l.pending_overflow as f64);
            field_num(state, c"pending_expired", l.pending_expired as f64);
        }
    }
    1
}

/// `configure(root)`: open the log and the crash file under `root`.
/// Returns true, or nil and an error.
unsafe fn work_configure(state: *mut State) -> c_int {
    // SAFETY: a live state.
    let Some(root) = (unsafe { lua::to_string(state, 1) }) else {
        // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
        return unsafe { fail(state, "configure needs the install root") };
    };
    match state::configure(&PathBuf::from(root)) {
        Ok(()) => {
            state::log(
                Level::Info,
                "agent configured",
                &[
                    ("pdb", Value::Str(&platform::pdb_id())),
                    ("pinned", Value::Bool(state::pinned())),
                    ("opens", Value::Int(crate::opens() as i64)),
                ],
            );
            // SAFETY: a live state with a free slot.
            unsafe { lua::lua_pushboolean(state, 1) };
            1
        }
        // SAFETY: as above.
        Err(e) => unsafe { fail(state, &e) },
    }
}

/// `log(level, message[, target[, key, value, ...]])`: one log record. With
/// no target, or `agent`, it goes to the agent's log and to the sidecar;
/// with another target, such as `hook` or `shim`, whose writer keeps its own
/// file, only to the sidecar. The pairs after the target are the record's
/// `data`: string keys, and values that are strings, numbers, booleans or
/// nil.
unsafe fn work_log(state: *mut State) -> c_int {
    // SAFETY: a live state.
    let level = unsafe { lua::to_string(state, 1) }.unwrap_or_default();
    // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
    let message = unsafe { lua::to_string(state, 2) }.unwrap_or_default();
    // SAFETY: as above.
    let target = unsafe { lua::to_string(state, 3) }.unwrap_or_else(|| "agent".into());
    let level = match level.as_str() {
        "error" => Level::Error,
        "warn" => Level::Warn,
        "debug" => Level::Debug,
        _ => Level::Info,
    };
    // SAFETY: a live state; reading a value never raises.
    let top = unsafe { lua::lua_gettop(state) };
    let mut pairs = Vec::new();
    let mut i = 4;
    while i < top {
        // SAFETY: as above; `i` and `i + 1` are on the stack.
        let key = unsafe { lua::to_string(state, i) };
        // SAFETY: as above.
        let value = unsafe { scalar(state, i + 1) };
        if let Some(key) = key {
            pairs.push((key, value));
        }
        i += 2;
    }
    if target == "agent" {
        let data: Vec<(&str, Value<'_>)> = pairs
            .iter()
            .map(|(k, v)| {
                let v = match v {
                    Lv::Bool(b) => Value::Bool(*b),
                    Lv::Number(n) => Value::Num(*n),
                    Lv::String(s) => Value::Str(core::str::from_utf8(s).unwrap_or("")),
                    _ => Value::Null,
                };
                (k.as_str(), v)
            })
            .collect();
        state::log(level, &message, &data);
    }
    let data = if pairs.is_empty() {
        Lv::Nil
    } else {
        Lv::Table(
            pairs
                .into_iter()
                .map(|(k, v)| (Lv::String(k.into_bytes()), v))
                .collect(),
        )
    };
    driver::log(level, &target, &message, data);
    0
}

/// The most fields `event` takes; pairs past it are ignored.
const EVENT_FIELDS: c_int = 16;

/// `event(id, code, value, ...)`: queue an `EVENT` record with the DCS event
/// ID and each `event_field` code and its value (spec/records.md). A code
/// that is not a whole number from 1 to 65,535 drops its pair. A value is
/// carried as an `lv` scalar: a table or a function becomes its type name.
/// Never raises. Returns the slot plus one of the table row a death event
/// removed (spec/records.md, "The object table"), else nothing.
unsafe fn work_event(state: *mut State) -> c_int {
    let started = std::time::Instant::now();
    // SAFETY: a live state; reading a value never raises.
    let id = unsafe { arg_num(state, 1, -1.0) };
    if !(0.0..=f64::from(u32::MAX)).contains(&id) || id.fract() != 0.0 {
        return 0;
    }
    // SAFETY: as above.
    let top = unsafe { lua::lua_gettop(state) }.min(1 + 2 * EVENT_FIELDS);
    let mut fields = Vec::new();
    let mut i = 2;
    while i < top {
        // SAFETY: as above; `i` and `i + 1` are on the stack.
        let code = unsafe { arg_num(state, i, -1.0) };
        if (1.0..=65535.0).contains(&code) && code.fract() == 0.0 {
            // SAFETY: as above.
            fields.push((code as u16, unsafe { scalar(state, i + 1) }));
        }
        i += 2;
    }
    let freed = driver::event(
        nanoprobe_transport::records::Event {
            id: id as u32,
            fields,
        },
        started,
    );
    if freed == 0 {
        return 0;
    }
    // SAFETY: a live state with a free slot.
    unsafe { lua::lua_pushinteger(state, freed as isize) };
    1
}

/// The value at `index` as an `lv` scalar: a string, number, boolean or nil,
/// and the type name of anything else.
unsafe fn scalar(state: *mut State, index: c_int) -> Lv {
    // SAFETY: a live state; `lua_type` accepts any index.
    match unsafe { lua::lua_type(state, index) } {
        lua::TNIL | lua::TNONE => Lv::Nil,
        lua::TBOOLEAN => {
            // SAFETY: as above.
            Lv::Bool(unsafe { lua::lua_toboolean(state, index) } != 0)
        }
        // SAFETY: as above; a number is read without converting it.
        lua::TNUMBER => Lv::Number(unsafe { lua::lua_tonumber(state, index) }),
        lua::TSTRING => {
            let mut len = 0;
            // SAFETY: the value is a string, so Lua returns `len` bytes that
            // live while it is on the stack.
            let p = unsafe { lua::lua_tolstring(state, index, &mut len) };
            if p.is_null() {
                Lv::Nil
            } else {
                // SAFETY: as above.
                Lv::String(unsafe { core::slice::from_raw_parts(p.cast::<u8>(), len) }.to_vec())
            }
        }
        // SAFETY: as above.
        t => Lv::Other(unsafe { lua::type_name(state, t) }),
    }
}

/// `start(dcs_version)`: open the link from `<root>/agent.toml` and
/// `<root>/token` (spec/config.md), after `configure`. Returns true and the
/// mapped file's path, or nil and an error. A second call changes nothing.
unsafe fn work_start(state: *mut State) -> c_int {
    let Some(root) = state::root() else {
        // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
        return unsafe { fail(state, "start needs configure first") };
    };
    // SAFETY: a live state.
    let version = unsafe { lua::to_string(state, 1) }.unwrap_or_default();
    match driver::start(&root, &version) {
        Ok(path) => {
            // SAFETY: a live state with two free slots.
            unsafe {
                lua::lua_pushboolean(state, 1);
                lua::push_str(state, &path);
            }
            2
        }
        Err(e) => {
            state::log(
                Level::Error,
                "link not opened",
                &[("error", Value::Str(&e))],
            );
            // SAFETY: as above.
            unsafe { fail(state, &e) }
        }
    }
}

/// The breadcrumb's high bits while the frame sweeps positions; its low bits
/// are the first object's ID.
const SWEEP_CRUMB: u64 = 0x5057 << 32;

/// `frame(real, model, paused, getter)`: the frame's link work and commands,
/// and, with `DCS.getUnitPositionByObjectId` as `getter`, its sweep of the
/// units' positions.
/// Returns 0, 1 when the hook should reload the shim, 2 when the agent is
/// disabled and the hook stops calling, or 3 when no link is open; and
/// whether the hook calls the shim's pump (a sampling frame, or a `server`
/// command waits), and whether a command with a `hook` handler waits, which
/// the hook runs with `command(true)` after the pump. The hook then calls
/// `finish`. Any other frame ends inside this call (spec/sampling.md, "The
/// frame").
unsafe fn work_frame(state: *mut State) -> c_int {
    // SAFETY: a live state.
    let real = unsafe { arg_num(state, 1, 0.0) };
    // SAFETY: as above.
    let model = unsafe { arg_num(state, 2, 0.0) };
    // SAFETY: as above; `lua_toboolean` reads any index.
    let paused = unsafe { lua::lua_toboolean(state, 3) } != 0;
    #[cfg(feature = "faults")]
    if driver::faults::take_raise() {
        state::log(Level::Warn, "injected fault: raise", &[]);
        // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
        return unsafe { raise_with(state, "injected Lua error") };
    }
    // The position getter by object ID, when this state has it: the frame
    // sweeps the positions it owes with it, each call under `lua_pcall`.
    // SAFETY: a live state; `lua_type` reads any index.
    let getter = unsafe { lua::lua_type(state, 4) } == lua::TFUNCTION;
    let mut crumbed = false;
    let mut read = |object: u64| -> Result<Option<[f64; 3]>, ()> {
        if !crumbed {
            crumbed = true;
            state::crumb(SWEEP_CRUMB | (object & 0xffff_ffff));
        }
        // SAFETY: a live state with the getter at 4 and the free slots Lua
        // guarantees a C function; the call runs under `lua_pcall`, and the
        // stack is reset after each row.
        unsafe {
            lua::lua_settop(state, 4);
            lua::lua_pushvalue(state, 4);
            lua::lua_pushnumber(state, object as f64);
            if lua::lua_pcall(state, 1, 3, 0) != 0 {
                lua::lua_settop(state, 4);
                return Err(());
            }
            let p = match (
                arg_finite(state, -3),
                arg_finite(state, -2),
                arg_finite(state, -1),
            ) {
                (Some(x), Some(y), Some(z)) => Some([x, y, z]),
                _ => None,
            };
            lua::lua_settop(state, 4);
            Ok(p)
        }
    };
    // SAFETY: as above.
    let shim = unsafe { lua::lua_toboolean(state, 5) } != 0;
    let (code, pump, hook) = driver::frame(real, model, paused, shim, getter.then_some(&mut read));
    // SAFETY: a live state with three free slots.
    unsafe {
        lua::lua_pushinteger(state, code as isize);
        lua::lua_pushboolean(state, c_int::from(pump));
        lua::lua_pushboolean(state, c_int::from(hook));
    }
    3
}

/// The encoded arguments `command` pushes, for [`push_staged`]. Single
/// threaded: only the sim thread calls the agent.
static STAGED: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// `command()`: in the shim's pump, the next command whose handler runs in
/// Lua, taken and admitted (spec/commands.md, "Handlers"). Returns its
/// opcode and a table of its arguments, or nothing. The table is built
/// under `lua_pcall`, so a push that raises leaves the command outstanding,
/// to be answered `LUA_ERROR` "no result".
unsafe fn work_command(state: *mut State) -> c_int {
    let mut args = std::mem::take(&mut *STAGED.lock().unwrap_or_else(|e| e.into_inner()));
    // SAFETY: a live state; reading a value never raises. The hook's state
    // passes true: it takes only a command whose handler runs there.
    let hook = unsafe { lua::lua_toboolean(state, 1) } != 0;
    let opcode = driver::command(&mut args, hook);
    *STAGED.lock().unwrap_or_else(|e| e.into_inner()) = args;
    let Some(opcode) = opcode else {
        return 0;
    };
    // SAFETY: a live state with the free slots Lua guarantees a C function;
    // the table is built under `lua_pcall`.
    unsafe {
        lua::lua_settop(state, 0);
        lua::lua_pushinteger(state, opcode as isize);
        lua::lua_pushcclosure(state, push_staged, 0);
        if lua::lua_pcall(state, 0, 1, 0) != 0 {
            lua::lua_settop(state, 0);
            return 0;
        }
    }
    2
}

/// Push the table [`STAGED`] encodes. Called only under `lua_pcall`: a push
/// may raise, and nothing here owns a value that a raise would skip.
unsafe extern "C" fn push_staged(state: *mut State) -> c_int {
    let guard = STAGED.lock().unwrap_or_else(|e| e.into_inner());
    let bytes: *const [u8] = &guard[..];
    drop(guard);
    // SAFETY: the bytes live in the static until the next `command`, which
    // cannot run during this call; the reader borrows them and owns nothing.
    let mut rd = nanoprobe_transport::codec::Reader::new(unsafe { &*bytes });
    // SAFETY: a live state; `push_lv` pushes one value.
    if unsafe { push_lv(state, &mut rd, 0) } {
        1
    } else {
        0
    }
}

/// The most table levels `push_lv` builds: the 16 the codec admits below the
/// arguments' root, and the `{name, args}` wrapper the driver puts around them.
const PUSH_DEPTH: u32 = 17;

/// Push the `lv` value at `rd`. False when it does not decode; nothing is
/// pushed then. Owns nothing: a raise from a push passes through it.
#[inline(never)]
unsafe fn push_lv(
    state: *mut State,
    rd: &mut nanoprobe_transport::codec::Reader<'_>,
    depth: u32,
) -> bool {
    use nanoprobe_transport::layout::lv_tag;
    let Ok(tag) = rd.u8() else { return false };
    // SAFETY: a live state; each branch pushes one value, and a table's
    // pairs are set as they come, so the stack grows by two at most.
    unsafe {
        match u64::from(tag) {
            lv_tag::NIL => lua::lua_pushnil(state),
            lv_tag::FALSE => lua::lua_pushboolean(state, 0),
            lv_tag::TRUE => lua::lua_pushboolean(state, 1),
            lv_tag::NUMBER => match rd.f64() {
                Ok(n) => lua::lua_pushnumber(state, n),
                Err(_) => return false,
            },
            lv_tag::STRING => {
                let Ok(n) = rd.u32() else { return false };
                let Ok(b) = rd.bytes(n as usize) else {
                    return false;
                };
                lua::lua_pushlstring(state, b.as_ptr().cast(), b.len());
            }
            lv_tag::TABLE if depth < PUSH_DEPTH => {
                let Ok(n) = rd.u32() else { return false };
                // A level uses 3 slots, 17 levels at most, grown 4 at a
                // time. Under the caller's `lua_pcall`, so a failed grow is a clean false.
                if lua::lua_checkstack(state, 4) == 0 {
                    return false;
                }
                lua::lua_createtable(state, 0, 0);
                for _ in 0..n {
                    if !push_lv(state, rd, depth + 1) {
                        lua::lua_settop(state, -2);
                        return false;
                    }
                    if !push_lv(state, rd, depth + 1) {
                        lua::lua_settop(state, -3);
                        return false;
                    }
                    lua::lua_rawset(state, -3);
                }
            }
            lv_tag::OTHER => {
                let Ok(name) = rd.str() else { return false };
                lua::lua_pushlstring(state, name.as_ptr().cast(), name.len());
            }
            _ => return false,
        }
    }
    true
}

/// What [`read_done`] read, for [`work_command_done`]: the code, the message,
/// the value, and whether the value was cut at the read limit. Single
/// threaded: only the sim thread calls the agent.
static DONE: Mutex<Option<Done>> = Mutex::new(None);

/// The code, message, value and cut flag of one `command_done`.
type Done = (Option<u64>, String, Lv, bool, (Vec<u64>, usize));

/// `command_done(code, message, value)`: the shim's handler finished the
/// command `command` returned. `value` is read as an `lv`, tables included,
/// 16 deep at most. The read runs under `lua_pcall`: it grows the stack once
/// and can raise only there, so a failed grow answers `LUA_ERROR`. A value cut
/// at the read limit is answered `TOO_LARGE` with a nil value.
unsafe fn work_command_done(state: *mut State) -> c_int {
    *DONE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let wants = driver::wants_destroyed();
    // SAFETY: a live state; `command_done` is called with four arguments,
    // the closure runs under `lua_pcall` with copies of them (nil for the
    // fourth unless the command's opcode reads it), and the stack has the
    // free slots Lua guarantees a C function.
    let failed = unsafe {
        lua::lua_settop(state, 4);
        lua::lua_pushcclosure(state, read_done, 0);
        lua::lua_pushvalue(state, 1);
        lua::lua_pushvalue(state, 2);
        lua::lua_pushvalue(state, 3);
        if wants {
            lua::lua_pushvalue(state, 4);
        } else {
            lua::lua_pushnil(state);
        }
        let status = lua::lua_pcall(state, 4, 0, 0);
        lua::lua_settop(state, 0);
        status != 0
    };
    let read = DONE.lock().unwrap_or_else(|e| e.into_inner()).take();
    let taken = match read {
        Some((code, message, value, cut, (destroyed, cut_ids))) if !failed => {
            let code = code.unwrap_or(result::LUA_ERROR);
            if cut && code == result::APPLIED {
                let why = format!(
                    "value over {READ_NODES} table entries or {} KiB of strings",
                    READ_BYTES / 1024
                );
                driver::command_done(result::TOO_LARGE, &why, Lv::Nil, &destroyed, cut_ids)
            } else {
                driver::command_done(code, &message, value, &destroyed, cut_ids)
            }
        }
        _ => {
            driver::command_done(
                result::LUA_ERROR,
                "the result could not be read",
                Lv::Nil,
                &[],
                0,
            );
            0
        }
    };
    // SAFETY: a live state; the stack was emptied above and has a free slot.
    unsafe { lua::lua_pushnumber(state, taken as f64) };
    1
}

/// Read `command_done`'s four arguments into [`DONE`]. Called only under
/// `lua_pcall`. The one call that can raise, `lua_checkstack` on a failed
/// grow, comes first, before this function owns a value; the reads after it
/// allocate nothing in Lua, and the message is read only when it is a string.
#[inline(never)]
unsafe extern "C" fn read_done(state: *mut State) -> c_int {
    // SAFETY: a live state; 3 slots per table level, 16 levels, and the
    // arguments.
    unsafe { lua::lua_checkstack(state, 64) };
    // SAFETY: as above; the stack has room for every read below.
    let done = unsafe {
        let code = arg_id(state, 1);
        let message = if lua::lua_type(state, 2) == lua::TSTRING {
            lua::to_string(state, 2).unwrap_or_default()
        } else {
            String::new()
        };
        let (value, cut) = read_lv(state, 3, 0);
        (code, message, value, cut, read_ids(state, 4))
    };
    *DONE.lock().unwrap_or_else(|e| e.into_inner()) = Some(done);
    0
}

/// The most object IDs `read_ids` reads.
const READ_IDS: usize = 4096;

/// The array of object IDs at `index`, in order, and how many past the limit
/// were cut; an entry that is not an ID
/// is skipped, and anything that is not a table reads as no IDs. Raw reads
/// of a table raise nothing and allocate nothing in Lua.
unsafe fn read_ids(state: *mut State, index: c_int) -> (Vec<u64>, usize) {
    let mut out = Vec::new();
    let cut;
    // SAFETY: a live state with the stack grown by the caller; `lua_type`
    // and `lua_objlen` never raise.
    unsafe {
        if lua::lua_type(state, index) != lua::TTABLE {
            return (out, 0);
        }
        let len = lua::lua_objlen(state, index);
        cut = len.saturating_sub(READ_IDS);
        let n = len.min(READ_IDS);
        for i in 1..=n {
            lua::lua_rawgeti(state, index, i as c_int);
            if let Some(id) = arg_id(state, -1) {
                out.push(id);
            }
            lua::lua_settop(state, -2);
        }
    }
    (out, cut)
}

/// The most table entries (key and value pairs) `read_lv` reads from one
/// return value. A table that
/// contains itself under several keys has k^16 paths, and the read runs on the
/// sim thread outside any budget. A value past the limit is cut, and the
/// command is answered `TOO_LARGE` (spec/commands.md, "Handlers").
const READ_NODES: u32 = 4096;
/// The most string bytes `read_lv` reads: a string that would pass the total
/// is not copied, and cuts the value.
const READ_BYTES: usize = 16 * 1024;

/// What one `read_lv` has read and the tables on its path.
struct ReadState {
    nodes: u32,
    bytes: usize,
    cut: bool,
    path: Vec<*const core::ffi::c_void>,
}

/// The value at `index` as an `lv`: a table by its pairs, 16 deep at most;
/// deeper tables and keys that are not strings, numbers or booleans read as
/// their type name or are skipped. A table on its own path reads as its type
/// name, and the read stops at [`READ_NODES`] values or [`READ_BYTES`] of
/// strings. The flag is true when the stop cut the value.
unsafe fn read_lv(state: *mut State, index: c_int, depth: u32) -> (Lv, bool) {
    let mut rs = ReadState {
        nodes: 0,
        bytes: 0,
        cut: false,
        path: Vec::new(),
    };
    // SAFETY: the caller's contract.
    let v = unsafe { read_node(state, index, depth, &mut rs) };
    (v, rs.cut)
}

/// One value of [`read_lv`]. The caller grew the stack for 16 levels under
/// `lua_pcall`; `lua_next` runs with a key it returned, and `lua_topointer`
/// and the type reads allocate nothing.
unsafe fn read_node(state: *mut State, index: c_int, depth: u32, rs: &mut ReadState) -> Lv {
    // SAFETY: a live state; `lua_type` accepts any index.
    if unsafe { lua::lua_type(state, index) } != lua::TTABLE || depth >= 16 {
        // SAFETY: as above. A string's length is read before it is copied.
        unsafe {
            if lua::lua_type(state, index) == lua::TSTRING
                && rs.bytes.saturating_add(lua::lua_objlen(state, index)) > READ_BYTES
            {
                rs.cut = true;
                return Lv::Nil;
            }
        }
        // SAFETY: as above.
        let v = unsafe { scalar(state, index) };
        if let Lv::String(s) = &v {
            rs.bytes = rs.bytes.saturating_add(s.len());
        }
        return v;
    }
    // SAFETY: a live state; `index` holds a table. A table index is made
    // absolute first, since pushes move relative ones.
    unsafe {
        let base = lua::lua_gettop(state);
        let at = if index < 0 { base + index + 1 } else { index };
        let id = lua::lua_topointer(state, at);
        if rs.path.contains(&id) {
            return Lv::Other(lua::type_name(state, lua::TTABLE));
        }
        rs.path.push(id);
        let mut pairs = Vec::new();
        lua::lua_pushnil(state);
        while lua::lua_next(state, at) != 0 {
            if rs.nodes >= READ_NODES || rs.bytes >= READ_BYTES {
                rs.cut = true;
                break;
            }
            let top = lua::lua_gettop(state);
            rs.nodes += 1;
            if lua::lua_type(state, top - 1) == lua::TSTRING
                && rs.bytes.saturating_add(lua::lua_objlen(state, top - 1)) > READ_BYTES
            {
                rs.cut = true;
                break;
            }
            let k = scalar(state, top - 1);
            if !matches!(k, Lv::Other(_) | Lv::Nil) {
                if let Lv::String(s) = &k {
                    rs.bytes = rs.bytes.saturating_add(s.len());
                }
                let v = read_node(state, top, depth + 1, rs);
                pairs.push((k, v));
            }
            lua::lua_settop(state, top - 1);
        }
        if rs.nodes > READ_NODES || rs.bytes > READ_BYTES {
            rs.cut = true;
        }
        // A cut read leaves the key on the stack.
        lua::lua_settop(state, base);
        rs.path.pop();
        Lv::Table(pairs)
    }
}

/// `finish()`: end the frame: account its time, publish its batch and store
/// the status block. The hook calls it on a sampling frame, after the
/// shim's pump; on a frame that `frame` ended it does nothing.
unsafe fn work_finish(_state: *mut State) -> c_int {
    driver::finish();
    0
}

/// `lifecycle(name, theater, mission, real, model)`: `loading`, `running`,
/// `paused` or `stopping`, with DCS real and model time in the callback.
/// Returns the mission generation, or nil when the name is unknown or no
/// link is open.
unsafe fn work_lifecycle(state: *mut State) -> c_int {
    // SAFETY: a live state.
    let name = unsafe { lua::to_string(state, 1) }.unwrap_or_default();
    // SAFETY: as above.
    let theater = unsafe { lua::to_string(state, 2) }.unwrap_or_default();
    // SAFETY: as above.
    let mission = unsafe { lua::to_string(state, 3) }.unwrap_or_default();
    // SAFETY: as above.
    let real = unsafe { arg_num(state, 4, 0.0) };
    // SAFETY: as above.
    let model = unsafe { arg_num(state, 5, 0.0) };
    match driver::lifecycle(&name, &theater, &mission, real, model) {
        // SAFETY: a live state with a free slot.
        Some(generation) => unsafe { lua::lua_pushinteger(state, generation as isize) },
        // SAFETY: as above.
        None => unsafe { lua::lua_pushnil(state) },
    }
    1
}

/// `thread()`: the OS ID of the calling thread.
unsafe fn work_thread(state: *mut State) -> c_int {
    // SAFETY: a live state with a free slot.
    unsafe { lua::lua_pushnumber(state, platform::thread_id() as f64) };
    1
}

/// `clock()`: seconds from the performance counter.
unsafe fn work_clock(state: *mut State) -> c_int {
    let seconds = platform::ticks() as f64 / platform::frequency() as f64;
    // SAFETY: a live state with a free slot.
    unsafe { lua::lua_pushnumber(state, seconds) };
    1
}

/// `qpc_cost(n)`: nanoseconds per performance-counter read, over `n` reads
/// in a tight loop, and the counter's frequency.
unsafe fn work_qpc_cost(state: *mut State) -> c_int {
    // SAFETY: a live state.
    let n = unsafe { arg_num(state, 1, 100_000.0) }.clamp(1.0, 10_000_000.0) as u64;
    let start = platform::ticks();
    let mut last = start;
    for _ in 0..n {
        last = core::hint::black_box(platform::ticks());
    }
    let elapsed = (last - start) as f64 / platform::frequency() as f64;
    // SAFETY: a live state with two free slots.
    unsafe {
        lua::lua_pushnumber(state, elapsed * 1e9 / n as f64);
        lua::lua_pushnumber(state, platform::frequency() as f64);
    }
    2
}

/// `crumb(code[, text])`: set the breadcrumb.
unsafe fn work_crumb(state: *mut State) -> c_int {
    // SAFETY: a live state.
    state::crumb(unsafe { arg_num(state, 1, 0.0) } as u64);
    // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
    if let Some(text) = unsafe { lua::to_string(state, 2) } {
        state::crumb_text_set(text.as_bytes());
    }
    0
}

/// `raise(message)`: raise a Lua error from the entry. The test of the
/// entry shape under the host's C runtime (E26).
unsafe fn work_raise(state: *mut State) -> c_int {
    // SAFETY: a live state.
    let message = unsafe { lua::to_string(state, 1) }.unwrap_or_else(|| "raised".into());
    // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
    unsafe { raise_with(state, &message) }
}

/// `call(f, ...)`: call `f` with the other arguments under `lua_pcall` from
/// C. Returns true and `f`'s results, or false and the error.
unsafe fn work_call(state: *mut State) -> c_int {
    // SAFETY: a live state; the function and its arguments are the whole
    // stack, which is what lua_pcall takes.
    unsafe {
        if lua::lua_type(state, 1) != lua::TFUNCTION {
            return raise_with(state, "call needs a function");
        }
        let nargs = lua::lua_gettop(state) - 1;
        state::crumb_text_set(b"call");
        let status = lua::lua_pcall(state, nargs, lua::MULTRET, 0);
        lua::lua_pushboolean(state, c_int::from(status == 0));
        lua::lua_insert(state, 1);
        lua::lua_gettop(state)
    }
}

/// `panic()`: a deliberate panic, caught by the entry, which disables the
/// agent.
unsafe fn work_panic(_state: *mut State) -> c_int {
    state::crumb_text_set(b"deliberate panic");
    panic!("deliberate panic for the crash capture test");
}

/// `fault()`: a deliberate access violation. DCS crashes.
unsafe fn work_fault(_state: *mut State) -> c_int {
    state::crumb_text_set(b"deliberate access violation");
    state::log(Level::Warn, "deliberate access violation", &[]);
    platform::fault();
    0
}

/// `inject(name[, arg])`: arm a fault for the next frame (driver::faults).
/// Returns true, or nil and why. A build without the `faults` feature
/// injects nothing and says so.
unsafe fn work_inject(state: *mut State) -> c_int {
    #[cfg(feature = "faults")]
    {
        // SAFETY: a live state.
        let name = unsafe { lua::to_string(state, 1) }.unwrap_or_default();
        // SAFETY: as above.
        let arg = unsafe { arg_num(state, 2, 0.0) };
        match driver::faults::inject(&name, arg) {
            Ok(()) => {
                state::log(
                    Level::Warn,
                    "fault armed",
                    &[("fault", Value::Str(&name)), ("arg", Value::Num(arg))],
                );
                // SAFETY: a live state with a free slot.
                unsafe { lua::lua_pushboolean(state, 1) };
                1
            }
            // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
            Err(e) => unsafe { fail(state, &e) },
        }
    }
    #[cfg(not(feature = "faults"))]
    {
        // SAFETY: as above.
        unsafe { fail(state, "this build has no fault injection") }
    }
}

/// The sink's running count and sum.
static SINK: Mutex<(u64, f64)> = Mutex::new((0, 0.0));

/// `sink(...)`: take numbers from Lua and add them up. The C side of the
/// measured sampling shape: a Lua loop calls each getter and passes the
/// scalars here. It cannot raise.
unsafe fn work_sink(state: *mut State) -> c_int {
    // SAFETY: a live state; `lua_tonumber` cannot raise.
    let top = unsafe { lua::lua_gettop(state) };
    let mut sum = 0.0;
    for i in 1..=top {
        // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
        sum += unsafe { lua::lua_tonumber(state, i) };
    }
    if let Ok(mut sink) = SINK.try_lock() {
        sink.0 += 1;
        sink.1 += sum;
    }
    0
}

/// `sink_take()`: the sink's count and sum, then zero them.
unsafe fn work_sink_take(state: *mut State) -> c_int {
    let (count, sum) = SINK
        .try_lock()
        .map(|mut s| core::mem::take(&mut *s))
        .unwrap_or_default();
    // SAFETY: a live state with two free slots.
    unsafe {
        lua::lua_pushnumber(state, count as f64);
        lua::lua_pushnumber(state, sum);
    }
    2
}

/// What `csweep` keeps in mode 2: each row's value and three results, and
/// each row's last three results for the moved test.
static ROWS: Mutex<(Vec<f64>, Vec<[f64; 3]>)> = Mutex::new((Vec::new(), Vec::new()));

/// `csweep(f, list, nres, mode)`: the C-driven shape (E5). For each value in
/// `list`, call `f(value)` under `lua_pcall`, read `nres` numbers, and add
/// them into the sink. Returns the rows done and the rows whose call raised.
/// `mode` 0 or absent stores a breadcrumb before each row, which reads the
/// clock; 1 stores none; 2 stores none and, with three results, does a
/// position row's work in place of the sum: it checks the three numbers,
/// compares them with the row's last, and keeps the value and the numbers.
unsafe fn work_csweep(state: *mut State) -> c_int {
    // SAFETY: a live state; every call that can raise runs under lua_pcall,
    // and the stack is reset to `base` after each row.
    unsafe {
        if lua::lua_type(state, 1) != lua::TFUNCTION || lua::lua_type(state, 2) != lua::TTABLE {
            return raise_with(state, "csweep needs a function and a list");
        }
        let nres = arg_num(state, 3, 1.0).clamp(0.0, 8.0) as c_int;
        let mode = arg_num(state, 4, 0.0) as u8;
        let n = lua::lua_objlen(state, 2);
        // The arguments only: a fourth argument must not count as a result.
        lua::lua_settop(state, 3);
        let base = lua::lua_gettop(state);
        let (mut rows, mut errors, mut sum) = (0u64, 0u64, 0.0);
        let mut kept = if mode == 2 {
            ROWS.try_lock().ok()
        } else {
            None
        };
        if let Some(kept) = kept.as_mut() {
            kept.0.clear();
            kept.1.resize(n, [0.0; 3]);
        }
        for i in 1..=n {
            if mode == 0 {
                state::crumb(i as u64);
            }
            lua::lua_pushvalue(state, 1);
            lua::lua_rawgeti(state, 2, i as c_int);
            let id = lua::lua_tonumber(state, -1);
            if lua::lua_pcall(state, 1, nres, 0) == 0 {
                match kept.as_mut() {
                    Some(kept) if nres == 3 => {
                        let p = [
                            lua::lua_tonumber(state, -3),
                            lua::lua_tonumber(state, -2),
                            lua::lua_tonumber(state, -1),
                        ];
                        if lua::lua_type(state, -3) == lua::TNUMBER
                            && p.iter().all(|v| v.is_finite())
                            && p != [0.0; 3]
                        {
                            let last = &mut kept.1[i - 1];
                            let d2: f64 = last.iter().zip(&p).map(|(a, b)| (a - b) * (a - b)).sum();
                            if d2 > 0.25 {
                                sum += 1.0;
                            }
                            *last = p;
                            kept.0.extend_from_slice(&[id, p[0], p[1], p[2]]);
                        }
                    }
                    _ => {
                        for k in 1..=nres {
                            sum += lua::lua_tonumber(state, -k);
                        }
                    }
                }
            } else {
                errors += 1;
            }
            lua::lua_settop(state, base);
            rows += 1;
        }
        drop(kept);
        if let Ok(mut sink) = SINK.try_lock() {
            sink.0 += rows;
            sink.1 += sum;
        }
        lua::lua_pushnumber(state, rows as f64);
        lua::lua_pushnumber(state, errors as f64);
        2
    }
}

/// The loopback pair for the socket probe: the sending side and the
/// receiving side.
static SOCKETS: Mutex<Option<(std::net::TcpStream, std::net::TcpStream)>> = Mutex::new(None);
/// Bytes sent, from a fixed buffer, and the receive buffer.
static SEND_BUF: [u8; 65536] = [0x5a; 65536];

/// `sock_open()`: a non-blocking loopback TCP pair inside this process.
unsafe fn work_sock_open(state: *mut State) -> c_int {
    let pair = (|| -> std::io::Result<_> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let sender = std::net::TcpStream::connect(listener.local_addr()?)?;
        let (receiver, _) = listener.accept()?;
        for s in [&sender, &receiver] {
            s.set_nonblocking(true)?;
            s.set_nodelay(true)?;
        }
        Ok((sender, receiver))
    })();
    match pair {
        Ok(pair) => {
            if let Ok(mut sockets) = SOCKETS.try_lock() {
                *sockets = Some(pair);
            }
            // SAFETY: a live state with a free slot.
            unsafe { lua::lua_pushboolean(state, 1) };
            1
        }
        // SAFETY: as above.
        Err(e) => unsafe { fail(state, &e.to_string()) },
    }
}

/// `sock_frame(bytes)`: one non-blocking `send` of `bytes` and one `recv`,
/// as the pump would make once a frame (E25). Returns the nanoseconds each
/// took and the bytes each moved; -1 bytes is "would block".
unsafe fn work_sock_frame(state: *mut State) -> c_int {
    use std::io::{Read as _, Write as _};
    // SAFETY: a live state.
    let bytes = unsafe { arg_num(state, 1, 512.0) }.clamp(0.0, SEND_BUF.len() as f64) as usize;
    let Ok(mut sockets) = SOCKETS.try_lock() else {
        // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
        return unsafe { fail(state, "busy") };
    };
    let Some((sender, receiver)) = sockets.as_mut() else {
        // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
        return unsafe { fail(state, "no socket pair; call sock_open") };
    };
    let mut buffer = [0u8; 65536];
    let freq = platform::frequency() as f64;
    let t0 = platform::ticks();
    let sent = sender.write(&SEND_BUF[..bytes]);
    let t1 = platform::ticks();
    let received = receiver.read(&mut buffer);
    let t2 = platform::ticks();
    let count = |r: std::io::Result<usize>| match r {
        Ok(n) => n as f64,
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => -1.0,
        Err(_) => -2.0,
    };
    // SAFETY: a live state with four free slots.
    unsafe {
        lua::lua_pushnumber(state, (t1 - t0) as f64 * 1e9 / freq);
        lua::lua_pushnumber(state, (t2 - t1) as f64 * 1e9 / freq);
        lua::lua_pushnumber(state, count(sent));
        lua::lua_pushnumber(state, count(received));
    }
    4
}

/// `sock_close()`: close the pair.
unsafe fn work_sock_close(_state: *mut State) -> c_int {
    if let Ok(mut sockets) = SOCKETS.try_lock() {
        *sockets = None;
    }
    0
}

/// The listener for the system-call frame probe.
static LISTENER: Mutex<Option<std::net::TcpListener>> = Mutex::new(None);

/// `sys_open()`: a non-blocking loopback listener that no one connects to.
unsafe fn work_sys_open(state: *mut State) -> c_int {
    let listener = (|| -> std::io::Result<_> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        Ok(listener)
    })();
    match listener {
        Ok(listener) => {
            let Ok(mut slot) = LISTENER.try_lock() else {
                // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
                return unsafe { fail(state, "busy") };
            };
            *slot = Some(listener);
            // SAFETY: a live state with a free slot.
            unsafe { lua::lua_pushboolean(state, 1) };
            1
        }
        // SAFETY: as above.
        Err(e) => unsafe { fail(state, &e.to_string()) },
    }
}

/// `sys_frame()`: the pump's system-call frame with no sidecar connected,
/// whose socket call is an `accept`; with one connected a `recv` replaces it,
/// which `sock_frame(0)` measures. Times, as `driver::frame_with` makes them,
/// an `accept` with nothing waiting, the kill-switch check, the reload check,
/// a log flush holding one record and a log flush holding none. Returns the
/// nanoseconds of each, then the accept's outcome: -1 is "would block".
unsafe fn work_sys_frame(state: *mut State) -> c_int {
    let Some(root) = state::root() else {
        // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
        return unsafe { fail(state, "not configured") };
    };
    let Ok(slot) = LISTENER.try_lock() else {
        // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
        return unsafe { fail(state, "busy") };
    };
    let Some(listener) = slot.as_ref() else {
        // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
        return unsafe { fail(state, "no listener; call sys_open") };
    };
    let freq = platform::frequency() as f64;
    let t0 = platform::ticks();
    let accepted = listener.accept();
    let t1 = platform::ticks();
    let disable = root.join("disable").exists();
    let t2 = platform::ticks();
    let reload = root.join("reload").exists();
    let t3 = platform::ticks();
    state::log(Level::Debug, "sys_frame probe", &[]);
    let t4 = platform::ticks();
    state::flush_log();
    let t5 = platform::ticks();
    state::flush_log();
    let t6 = platform::ticks();
    std::hint::black_box((disable, reload));
    let outcome = match accepted {
        Ok(_) => 1.0,
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => -1.0,
        Err(_) => -2.0,
    };
    let ns = |a: i64, b: i64| (b - a) as f64 * 1e9 / freq;
    // SAFETY: a live state with six free slots.
    unsafe {
        lua::lua_pushnumber(state, ns(t0, t1));
        lua::lua_pushnumber(state, ns(t1, t2));
        lua::lua_pushnumber(state, ns(t2, t3));
        lua::lua_pushnumber(state, ns(t4, t5));
        lua::lua_pushnumber(state, ns(t5, t6));
        lua::lua_pushnumber(state, outcome);
    }
    6
}

/// `sys_close()`: close the listener.
unsafe fn work_sys_close(_state: *mut State) -> c_int {
    if let Ok(mut slot) = LISTENER.try_lock() {
        *slot = None;
    }
    0
}

/// The mapped file for the E24 probe.
static MAPPING: Mutex<Option<MappedFile>> = Mutex::new(None);
/// Words of the probe's mapping in use: 0 magic, 1 sequence, 2..10 the
/// payload the agent writes, 16 the peer's echo, 17 the peer's sequence.
const MAP_WORDS: usize = 64;
/// The probe's file: the transport's mapped file holds at least a status
/// block, so the file is that long and the peer maps its first 512 bytes.
const MAP_BYTES: u64 = 4096;
const MAP_MAGIC: u64 = 0x6e70_6d61_7070_6564;

/// `map_open(path)`: map the file at `path`, 4,096 bytes, and write the
/// magic.
unsafe fn work_map_open(state: *mut State) -> c_int {
    // SAFETY: a live state.
    let Some(path) = (unsafe { lua::to_string(state, 1) }) else {
        // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
        return unsafe { fail(state, "map_open needs a path") };
    };
    match MappedFile::create(&PathBuf::from(&path), MAP_BYTES) {
        Ok(mapping) => {
            mapping.store(8, 0, Ordering::Release);
            mapping.store(0, MAP_MAGIC, Ordering::Release);
            if let Ok(mut slot) = MAPPING.try_lock() {
                *slot = Some(mapping);
            }
            // SAFETY: a live state with a free slot.
            unsafe { lua::lua_pushboolean(state, 1) };
            1
        }
        // SAFETY: as above.
        Err(e) => unsafe { fail(state, &format!("{path}: {e}")) },
    }
}

/// `map_frame(value)`: write `value` into the eight payload words under a
/// sequence lock, and return the peer's echo and its sequence word.
unsafe fn work_map_frame(state: *mut State) -> c_int {
    use core::sync::atomic::Ordering::{Acquire, Release};
    // SAFETY: a live state.
    let value = unsafe { arg_num(state, 1, 0.0) } as u64;
    let Ok(slot) = MAPPING.try_lock() else {
        // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
        return unsafe { fail(state, "busy") };
    };
    let Some(m) = slot.as_ref() else {
        // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
        return unsafe { fail(state, "no mapping; call map_open") };
    };
    let seq = m.load(8, Acquire);
    m.store(8, seq + 1, Release);
    for word in 2..10 {
        m.store(word * 8, value, Release);
    }
    m.store(8, seq + 2, Release);
    let echo = m.load(16 * 8, Acquire);
    let peer = m.load(17 * 8, Acquire);
    // SAFETY: a live state with two free slots.
    unsafe {
        lua::lua_pushnumber(state, echo as f64);
        lua::lua_pushnumber(state, peer as f64);
    }
    2
}

/// `map_load(index)`: one word of the mapping.
unsafe fn work_map_load(state: *mut State) -> c_int {
    // SAFETY: a live state.
    let index = unsafe { arg_num(state, 1, 0.0) };
    let Ok(slot) = MAPPING.try_lock() else {
        // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
        return unsafe { fail(state, "busy") };
    };
    let (Some(m), true) = (slot.as_ref(), (0.0..MAP_WORDS as f64).contains(&index)) else {
        // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
        return unsafe { fail(state, "no mapping, or index out of range") };
    };
    let value = m.load(index as usize * 8, Ordering::Acquire);
    // SAFETY: a live state with a free slot.
    unsafe { lua::lua_pushnumber(state, value as f64) };
    1
}

/// `map_close()`: unmap.
unsafe fn work_map_close(_state: *mut State) -> c_int {
    if let Ok(mut slot) = MAPPING.try_lock() {
        *slot = None;
    }
    0
}

// ---- sampling (spec/sampling.md) ----------------------------------------
//
// The shim's pump calls these from the `server` state. Every slot number
// that crosses into Lua is the table's slot plus one, so the shim can use it
// as an array index.

/// The argument at `index` as a whole number from 0 to 2^53, or None.
unsafe fn arg_id(state: *mut State, index: c_int) -> Option<u64> {
    // SAFETY: a live state; `lua_type` and `lua_tonumber` never raise.
    let n = unsafe { arg_num(state, index, -1.0) };
    ((0.0..9.007_199_254_740_992e15).contains(&n) && n.fract() == 0.0).then_some(n as u64)
}

/// The argument at `index` as a slot, from the slot plus one Lua holds.
unsafe fn arg_slot(state: *mut State, index: c_int) -> Option<u32> {
    // SAFETY: as for `arg_id`.
    let n = unsafe { arg_id(state, index) }?;
    (1..=u64::from(u32::MAX))
        .contains(&n)
        .then(|| (n - 1) as u32)
}

/// The argument at `index` as a finite number, or None.
unsafe fn arg_finite(state: *mut State, index: c_int) -> Option<f64> {
    // SAFETY: a live state; `lua_type` never raises.
    if unsafe { lua::lua_type(state, index) } != lua::TNUMBER {
        return None;
    }
    // SAFETY: as above.
    let n = unsafe { lua::lua_tonumber(state, index) };
    n.is_finite().then_some(n)
}

/// Push a boolean and return 1.
unsafe fn push_bool(state: *mut State, b: bool) -> c_int {
    // SAFETY: a live state with a free slot.
    unsafe { lua::lua_pushboolean(state, c_int::from(b)) };
    1
}

/// `deaths(id, ...)`: the DCS event IDs whose events remove a table row.
unsafe fn work_deaths(state: *mut State) -> c_int {
    // SAFETY: a live state; reading a value never raises.
    let top = unsafe { lua::lua_gettop(state) }.min(16);
    let ids = (1..=top)
        // SAFETY: as above.
        .filter_map(|i| unsafe { arg_id(state, i) })
        .filter_map(|n| u32::try_from(n).ok())
        .collect();
    driver::set_deaths(ids);
    0
}

/// `reads(which, id, ...)`: the DCS event IDs that ask for a read: `which`
/// 1 for a read of the target's life, 2 for a read of the initiator's
/// ammunition, 3 for the births the shim keeps for the next pump.
unsafe fn work_reads(state: *mut State) -> c_int {
    // SAFETY: a live state; reading a value never raises.
    let which = unsafe { arg_id(state, 1) }.unwrap_or(0) as u32;
    // SAFETY: as above.
    let top = unsafe { lua::lua_gettop(state) }.min(17);
    let ids = (2..=top)
        // SAFETY: as above.
        .filter_map(|i| unsafe { arg_id(state, i) })
        .filter_map(|n| u32::try_from(n).ok())
        .collect();
    driver::set_reads(which, ids);
    0
}

/// `spot(id, x, y, z)`: a static's point, read when its row is added and
/// when an enumeration finds it.
unsafe fn work_spot(state: *mut State) -> c_int {
    // SAFETY: a live state; reading a value never raises.
    unsafe {
        if let (Some(object), Some(x), Some(y), Some(z)) = (
            arg_id(state, 1),
            arg_finite(state, 2),
            arg_finite(state, 3),
            arg_finite(state, 4),
        ) {
            driver::spot(object, [x, y, z]);
        }
    }
    0
}

/// `pump_begin(births, airbases, heap_kib)`: the pump begins. Returns the
/// mission generation and whether to sample, or nothing with no link.
unsafe fn work_pump_begin(state: *mut State) -> c_int {
    // SAFETY: a live state.
    let births = unsafe { arg_id(state, 1) }.unwrap_or(0) as usize;
    // SAFETY: as above.
    let airbases = unsafe { arg_id(state, 2) }.unwrap_or(0) as usize;
    // SAFETY: as above.
    let heap = unsafe { arg_num(state, 3, -1.0) };
    let Some((generation, sample)) = driver::pump_begin(births, airbases, heap) else {
        return 0;
    };
    // SAFETY: a live state with two free slots.
    unsafe {
        lua::lua_pushinteger(state, generation as isize);
        lua::lua_pushboolean(state, c_int::from(sample));
    }
    2
}

/// The object IDs `checks` hands out, kept to avoid an allocation.
static CHECKS: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// `checks(buffer)`: the objects whose existence the pump checks now
/// (spec/records.md, "The object table"). Writes their object IDs into
/// `buffer`, an array with room for them, and returns how many.
unsafe fn work_checks(state: *mut State) -> c_int {
    // SAFETY: a live state.
    if unsafe { lua::lua_type(state, 1) } != lua::TTABLE {
        return 0;
    }
    // SAFETY: as above; the argument is a table.
    let room = unsafe { lua::lua_objlen(state, 1) };
    let Ok(mut ids) = CHECKS.try_lock() else {
        return 0;
    };
    let n = driver::checks(&mut ids).min(room);
    for (i, id) in ids.iter().take(n).enumerate() {
        // SAFETY: a live state with a free slot; index i + 1 exists in the
        // table at 1, so the raw set does not allocate.
        unsafe {
            lua::lua_pushnumber(state, *id as f64);
            lua::lua_rawseti(state, 1, (i + 1) as c_int);
        }
    }
    // SAFETY: a live state with a free slot.
    unsafe { lua::lua_pushinteger(state, n as isize) };
    1
}

/// `gone(id, dead_ok, dead, life_ok, life)`: the object no longer exists.
/// `dead_ok` and `life_ok` say whether each read returned, and `dead` and
/// `life` are what it returned: for a unit `Unit.isDead` and `Unit.getLife`,
/// for a static nothing and `StaticObject.getLife`. Returns the slot plus
/// one of the row removed, or nothing.
unsafe fn work_gone(state: *mut State) -> c_int {
    // SAFETY: a live state; reading a value never raises.
    unsafe {
        let Some(object) = arg_id(state, 1) else {
            return 0;
        };
        let dead = if lua::lua_toboolean(state, 2) == 0 {
            driver::Read::Raised
        } else if lua::lua_type(state, 3) == lua::TBOOLEAN {
            driver::Read::Value(Some(lua::lua_toboolean(state, 3) != 0))
        } else {
            driver::Read::Value(None)
        };
        let life = if lua::lua_toboolean(state, 4) == 0 {
            driver::Read::Raised
        } else {
            driver::Read::Value(arg_finite(state, 5))
        };
        let freed = driver::gone(object, dead, life);
        if freed == 0 {
            return 0;
        }
        lua::lua_pushinteger(state, freed as isize);
    }
    1
}

/// `alive(id, sure)`: the existence check of the object did not find it
/// gone. `sure` says `isExist` returned true; otherwise it raised, or the
/// shim holds no object for the ID, and nothing is known.
unsafe fn work_alive(state: *mut State) -> c_int {
    // SAFETY: a live state; reading a value never raises.
    unsafe {
        if let Some(object) = arg_id(state, 1) {
            driver::alive(object, lua::lua_toboolean(state, 2) != 0);
        }
    }
    0
}

/// The slots of the batch `plan` hands out, and the values it writes, kept
/// to avoid an allocation.
static PLAN: Mutex<(Vec<u32>, Vec<i64>)> = Mutex::new((Vec::new(), Vec::new()));

/// The tier codes of the batches the DLL reads itself.
const TIER_SUBSCRIBED: u8 = 3;
const TIER_POSITIONS: u8 = 4;
const TIER_VITALS: u8 = 6;

/// The shim's function list, by index: the position getter by object ID
/// (false when DCS lacks it), a unit's `getLife` and `getFuel`, a static's
/// `getLife`, and `getPoint`.
const FN_BY_ID: c_int = 1;
const FN_UNIT_LIFE: c_int = 2;
const FN_UNIT_FUEL: c_int = 3;
const FN_STATIC_LIFE: c_int = 4;
const FN_POINT: c_int = 5;

/// The number at field `key` of the table on top of the stack.
///
/// # Safety
///
/// A live state with a table on top and a free slot.
unsafe fn raw_field(state: *mut State, key: &str) -> Option<f64> {
    // SAFETY: the caller's contract; a raw read calls no metamethod.
    unsafe {
        lua::push_str(state, key);
        lua::lua_rawget(state, -2);
        let v = (lua::lua_type(state, -1) == lua::TNUMBER).then(|| lua::lua_tonumber(state, -1));
        let top = lua::lua_gettop(state);
        lua::lua_settop(state, top - 1);
        v
    }
}

/// Call the function at `which` of the list at 3 with the value on top of
/// the stack, which it pops, for `nres` results. False when the call raised;
/// the error value is then on top.
///
/// # Safety
///
/// A live state with the function list at 3, one argument on top and free
/// slots for the results.
unsafe fn call_listed(state: *mut State, which: c_int, nres: c_int) -> bool {
    // SAFETY: the caller's contract; the call runs under `lua_pcall`.
    unsafe {
        lua::lua_rawgeti(state, 3, which);
        lua::lua_insert(state, -2);
        lua::lua_pcall(state, 1, nres, 0) == 0
    }
}

/// Read a batch's rows from the DLL: each getter under `lua_pcall`, each
/// row's values straight to the sampler. For positions without attitude and
/// for vitals. A read that raises leaves its row out, as the shim's own
/// loops do, and ends the batch. Returns true when the budget is spent.
///
/// # Safety
///
/// A live state with the shim's object array at 2 and its function list at
/// 3; `slots` and `values` are the batch [`driver::plan`] just returned.
unsafe fn rows_here(state: *mut State, tier: u8, slots: &[u32], values: &[i64]) -> bool {
    // SAFETY: the caller's contract; every engine call runs under
    // `lua_pcall`, and the stack is reset to `base` after each row.
    unsafe {
        let base = lua::lua_gettop(state);
        state::crumb(u64::from(tier) << 32 | u64::from(slots.first().copied().unwrap_or(0)));
        lua::lua_rawgeti(state, 3, FN_BY_ID);
        let by_id = lua::lua_type(state, -1) == lua::TFUNCTION;
        lua::lua_settop(state, base);
        for (&slot, &value) in slots.iter().zip(values) {
            let stop = if tier == TIER_VITALS {
                lua::lua_rawgeti(state, 2, slot as c_int + 1);
                if lua::lua_type(state, -1) != lua::TTABLE {
                    driver::vitals(slot, None, None)
                } else {
                    let unit = value > 0;
                    lua::lua_pushvalue(state, -1);
                    let life = if unit { FN_UNIT_LIFE } else { FN_STATIC_LIFE };
                    if !call_listed(state, life, 1) {
                        lua::lua_settop(state, base);
                        driver::suspect(slot);
                        return false;
                    }
                    let life = arg_finite(state, -1);
                    lua::lua_settop(state, base + 1);
                    let mut fuel = None;
                    if unit {
                        if !call_listed(state, FN_UNIT_FUEL, 1) {
                            lua::lua_settop(state, base);
                            driver::suspect(slot);
                            return false;
                        }
                        fuel = arg_finite(state, -1);
                    }
                    driver::vitals(slot, life, fuel)
                }
            } else if value > 0 && by_id {
                lua::lua_pushnumber(state, value as f64);
                if !call_listed(state, FN_BY_ID, 3) {
                    lua::lua_settop(state, base);
                    driver::suspect_next();
                    return false;
                }
                let p = match (
                    arg_finite(state, -3),
                    arg_finite(state, -2),
                    arg_finite(state, -1),
                ) {
                    (Some(x), Some(y), Some(z)) => Some([x, y, z]),
                    _ => None,
                };
                driver::position(p)
            } else {
                lua::lua_rawgeti(state, 2, slot as c_int + 1);
                let mut p = None;
                if lua::lua_type(state, -1) == lua::TTABLE {
                    if !call_listed(state, FN_POINT, 1) {
                        lua::lua_settop(state, base);
                        driver::suspect_next();
                        return false;
                    }
                    if lua::lua_type(state, -1) == lua::TTABLE
                        && let (Some(x), Some(y), Some(z)) = (
                            raw_field(state, "x"),
                            raw_field(state, "y"),
                            raw_field(state, "z"),
                        )
                        && x.is_finite()
                        && y.is_finite()
                        && z.is_finite()
                    {
                        p = Some([x, y, z]);
                    }
                }
                driver::position(p)
            };
            lua::lua_settop(state, base);
            if stop {
                return true;
            }
        }
        false
    }
}

/// `plan(buf, objs, fns)`: the next batch the shim reads. With the shim's
/// object array and function list, the DLL reads the batches of positions
/// without attitude and of vitals itself ([`rows_here`]) and returns the
/// next batch of another tier. Writes into `buf[1..n]`, for the row tiers,
/// each row's value ([`nanoprobe_agent_link::Sampler::buffer_values`]: a
/// unit's object ID for positions without attitude, minus the slot plus one
/// for a static, else the slot plus one), and returns the tier code, the row count and the
/// flag; nothing when the budget is spent. `buf` is the shim's array, made
/// with at least 256 entries: only entries that exist are written, so the
/// table never grows and the write cannot raise.
unsafe fn work_plan(state: *mut State) -> c_int {
    // SAFETY: a live state.
    if unsafe { lua::lua_type(state, 1) } != lua::TTABLE {
        return 0;
    }
    // SAFETY: as above; the argument is a table.
    let room = unsafe { lua::lua_objlen(state, 1) };
    let Ok(mut plan) = PLAN.try_lock() else {
        return 0;
    };
    let (slots, values) = &mut *plan;
    // SAFETY: a live state; `lua_type` reads any index.
    let direct =
        unsafe { lua::lua_type(state, 2) == lua::TTABLE && lua::lua_type(state, 3) == lua::TTABLE };
    let (tier, mut rows, flag) = loop {
        let Some((tier, rows, flag)) = driver::plan(slots, values) else {
            return 0;
        };
        let here = direct
            && (tier == TIER_VITALS
                || (!flag && (tier == TIER_SUBSCRIBED || tier == TIER_POSITIONS)));
        if !here {
            break (tier, rows, flag);
        }
        // SAFETY: a live state with the shim's object array at 2 and its
        // function list at 3.
        if unsafe { rows_here(state, tier, slots, values) } {
            // The budget is spent: the pump ends.
            return 0;
        }
    };
    if !values.is_empty() {
        // A batch longer than the buffer is cut: the rows not written are
        // never read, so the sampler counts them as not done.
        rows = values.len().min(room);
        for (i, v) in values.iter().take(rows).enumerate() {
            // SAFETY: a live state with a free slot; index i + 1 exists in
            // the table at 1, so the raw set does not allocate.
            unsafe {
                lua::lua_pushinteger(state, *v as isize);
                lua::lua_rawseti(state, 1, (i + 1) as c_int);
            }
        }
    }
    // SAFETY: a live state with three free slots.
    unsafe {
        lua::lua_pushinteger(state, isize::from(tier));
        lua::lua_pushinteger(state, rows as isize);
        lua::lua_pushboolean(state, c_int::from(flag));
    }
    3
}

/// `pump_end()`: the pump ends.
unsafe fn work_pump_end(_state: *mut State) -> c_int {
    driver::pump_end();
    0
}

/// `pos(x, y, z)`: the position of the batch's next row; the three numbers
/// absent when the read failed. Returns true to stop.
unsafe fn work_pos(state: *mut State) -> c_int {
    // SAFETY: a live state; every read is of an argument and never raises.
    let p = unsafe {
        (
            arg_finite(state, 1),
            arg_finite(state, 2),
            arg_finite(state, 3),
        )
    };
    let p = match p {
        (Some(x), Some(y), Some(z)) => Some([x, y, z]),
        _ => None,
    };
    // SAFETY: as above.
    unsafe { push_bool(state, driver::position(p)) }
}

/// `att(slot, xx, xy, xz, yx, yy, yz)`: attitude from `getPosition()`'s
/// forward and up vectors.
unsafe fn work_att(state: *mut State) -> c_int {
    // SAFETY: a live state.
    let Some(slot) = (unsafe { arg_slot(state, 1) }) else {
        return 0;
    };
    let mut v = [0.0; 6];
    for (i, out) in v.iter_mut().enumerate() {
        // SAFETY: as above.
        match unsafe { arg_finite(state, i as c_int + 2) } {
            Some(n) => *out = n,
            None => return 0,
        }
    }
    driver::attitude(slot, [v[0], v[1], v[2]], [v[3], v[4], v[5]]);
    0
}

/// `stopped()`: a stopped row's attitude done. Returns true to stop.
unsafe fn work_stopped(state: *mut State) -> c_int {
    // SAFETY: a live state.
    unsafe { push_bool(state, driver::stopped()) }
}

/// `vit(slot, life, fuel)`: life and fuel; fuel absent for a static.
/// Returns true to stop.
unsafe fn work_vit(state: *mut State) -> c_int {
    // SAFETY: a live state.
    let Some(slot) = (unsafe { arg_slot(state, 1) }) else {
        // SAFETY: as above.
        return unsafe { push_bool(state, false) };
    };
    // SAFETY: as above.
    let (life, fuel) = unsafe { (arg_finite(state, 2), arg_finite(state, 3)) };
    // SAFETY: as above.
    unsafe { push_bool(state, driver::vitals(slot, life, fuel)) }
}

/// The most ammunition entries `ammo` takes.
const AMMO_MAX: c_int = 64;

/// `ammo(slot, type, count, ...)`: a unit's ammunition. Returns true to
/// stop.
unsafe fn work_ammo(state: *mut State) -> c_int {
    // SAFETY: a live state.
    let Some(slot) = (unsafe { arg_slot(state, 1) }) else {
        // SAFETY: as above.
        return unsafe { push_bool(state, false) };
    };
    // SAFETY: as above.
    let top = unsafe { lua::lua_gettop(state) }.min(1 + 2 * AMMO_MAX);
    let mut items = Vec::new();
    let mut i = 2;
    while i < top {
        // SAFETY: as above; `i` and `i + 1` are on the stack.
        if let (Lv::String(t), Some(c)) = unsafe { (scalar(state, i), arg_finite(state, i + 1)) } {
            items.push((t, c));
        }
        i += 2;
    }
    // SAFETY: as above.
    unsafe { push_bool(state, driver::ammo(slot, &items)) }
}

/// A table's pairs.
type Pairs = Vec<(Lv, Lv)>;

/// The most stock entries `airbase` takes.
const STOCK_MAX: c_int = 4096;

/// `airbase(index, id, coalition, name, has_stock, category, key, value,
/// ...)`: one airbase of the batch and, when `has_stock` is true, its
/// warehouse's stock as triples of category (`weapon`, `aircraft`,
/// `liquids`), name or number, and amount. Returns true to stop.
unsafe fn work_airbase(state: *mut State) -> c_int {
    // SAFETY: a live state.
    let Some(index) = (unsafe { arg_slot(state, 1) }) else {
        // SAFETY: as above.
        return unsafe { push_bool(state, false) };
    };
    // SAFETY: as above.
    let object = unsafe { arg_id(state, 2) }.unwrap_or(0);
    // SAFETY: as above.
    let coalition = unsafe { arg_id(state, 3) }.unwrap_or(0) as u32;
    // SAFETY: as above.
    let name = unsafe { lua::to_string(state, 4) }.unwrap_or_default();
    // SAFETY: as above; `lua_toboolean` reads any index.
    let has_stock = unsafe { lua::lua_toboolean(state, 5) } != 0;
    let stock = has_stock.then(|| {
        // SAFETY: as above.
        let top = unsafe { lua::lua_gettop(state) }.min(5 + 3 * STOCK_MAX);
        let mut cats: Vec<(Vec<u8>, Pairs)> = Vec::new();
        let mut i = 6;
        while i + 2 <= top {
            // SAFETY: as above; `i` to `i + 2` are on the stack.
            let (cat, key, value) =
                unsafe { (scalar(state, i), scalar(state, i + 1), scalar(state, i + 2)) };
            if let (Lv::String(cat), Lv::Number(_) | Lv::String(_), Lv::Number(_)) =
                (&cat, &key, &value)
            {
                match cats.iter_mut().find(|(c, _)| c == cat) {
                    Some((_, items)) => items.push((key, value)),
                    None => cats.push((cat.clone(), vec![(key, value)])),
                }
            }
            i += 3;
        }
        Lv::Table(
            cats.into_iter()
                .map(|(c, items)| (Lv::String(c), Lv::Table(items)))
                .collect(),
        )
    });
    let a = nanoprobe_transport::records::Airbase {
        object,
        coalition,
        airbase: name,
    };
    // SAFETY: as above.
    unsafe { push_bool(state, driver::airbase(index, a, stock)) }
}

/// `item()`: a birth or an enumeration item done. Returns true to stop.
unsafe fn work_item(state: *mut State) -> c_int {
    // SAFETY: a live state.
    unsafe { push_bool(state, driver::item()) }
}

/// `seen(id)`: an object the running enumeration found. Returns its slot
/// plus one, 0 when the table lacks it, or nil when it died in this
/// generation or no enumeration runs; and whether to stop. Unless it returns
/// 0, it also counts the item, so the shim makes one call.
unsafe fn work_seen(state: *mut State) -> c_int {
    // SAFETY: a live state.
    let (found, stop) = match unsafe { arg_id(state, 1) } {
        Some(id) => driver::seen(id),
        None => (None, false),
    };
    // SAFETY: a live state with two free slots (DCS gives a C function
    // LUA_MINSTACK).
    unsafe {
        match found {
            Some(slot) => lua::lua_pushinteger(state, slot as isize),
            None => lua::lua_pushnil(state),
        }
        lua::lua_pushboolean(state, c_int::from(stop));
    }
    2
}

/// `object(id, group, kind, category, coalition, country, life0, name,
/// type, group_name, enumerated, movable)`: add a row; `movable` for a
/// static that can move (cargo). Returns its slot plus one, or
/// 0 when the table has it or it died in this generation.
unsafe fn work_object(state: *mut State) -> c_int {
    // SAFETY: a live state; every read below is of an argument and never
    // raises.
    let found = unsafe {
        driver::Found {
            object: arg_id(state, 1).unwrap_or(0),
            group: arg_id(state, 2).unwrap_or(0),
            kind: arg_id(state, 3).unwrap_or(0) as u32,
            category: arg_id(state, 4).unwrap_or(0) as u32,
            coalition: arg_id(state, 5).unwrap_or(0) as u32,
            country: arg_id(state, 6).unwrap_or(0) as u32,
            life0: arg_finite(state, 7).unwrap_or(0.0) as f32,
            name: lua::to_string(state, 8).unwrap_or_default(),
            type_name: lua::to_string(state, 9).unwrap_or_default(),
            group_name: lua::to_string(state, 10).unwrap_or_default(),
            movable: lua::lua_toboolean(state, 12) != 0,
        }
    };
    // SAFETY: as above.
    let enumerated = unsafe { lua::lua_toboolean(state, 11) } != 0;
    let slot = driver::add(found, enumerated);
    // SAFETY: a live state with a free slot.
    unsafe { lua::lua_pushinteger(state, slot as isize) };
    1
}

/// `wants(id)`: whether the table would take the object, and whether it
/// died in this generation.
unsafe fn work_wants(state: *mut State) -> c_int {
    // SAFETY: a live state.
    let (wants, died) = unsafe { arg_id(state, 1) }.map_or((false, false), driver::wants);
    // SAFETY: as above.
    unsafe {
        push_bool(state, wants);
        push_bool(state, died)
    };
    2
}

/// `enum_done()`: the running enumeration read everything.
unsafe fn work_enum_done(_state: *mut State) -> c_int {
    driver::enumeration_done();
    0
}

/// `suspect([slot])`: a getter raised on the row, or with no slot on the
/// batch's next row.
unsafe fn work_suspect(state: *mut State) -> c_int {
    // SAFETY: a live state.
    match unsafe { arg_slot(state, 1) } {
        Some(slot) => driver::suspect(slot),
        None => driver::suspect_next(),
    }
    0
}

/// The argument at `index` as an unsigned 32-bit number: 0 when it is absent,
/// not a number, negative or not finite, and the nearest bound otherwise.
unsafe fn arg_u32(state: *mut State, index: c_int) -> u32 {
    // SAFETY: as for `arg_num`.
    let n = unsafe { arg_num(state, index, 0.0) };
    if n.is_finite() {
        n.clamp(0.0, f64::from(u32::MAX)) as u32
    } else {
        0
    }
}

/// The argument at `index` as an unsigned 64-bit count: 0 when it is absent,
/// not a number, negative or not finite.
unsafe fn arg_count(state: *mut State, index: c_int) -> u64 {
    // SAFETY: as for `arg_num`.
    let n = unsafe { arg_num(state, index, 0.0) };
    if n.is_finite() { n.max(0.0) as u64 } else { 0 }
}

/// The argument at `index` as text, empty when it is not a string or a number.
unsafe fn arg_text(state: *mut State, index: c_int) -> String {
    // SAFETY: as for `arg_num`.
    unsafe { lua::to_string(state, index) }.unwrap_or_default()
}

/// `player(event, id, side, seat, client, name, slot, text)`: queue a
/// `PLAYER` record from a hook callback (spec/records.md, "Layout: PLAYER").
/// An `event` outside the `player_event` codes 1 to 5 writes nothing. Never
/// raises.
unsafe fn work_player(state: *mut State) -> c_int {
    // SAFETY: a live state; every read is of an argument and never raises.
    let record = unsafe {
        Player {
            event: arg_u32(state, 1),
            player: arg_u32(state, 2),
            side: arg_u32(state, 3),
            seat: arg_u32(state, 4),
            client: arg_text(state, 5),
            name: arg_text(state, 6),
            slot: arg_text(state, 7),
            text: arg_text(state, 8),
        }
    };
    if (1..=5).contains(&record.event) {
        driver::player(&record);
    }
    0
}

/// The entity a platform name (spec/world.md, "Names") ends in.
fn entity_of_name(name: &str) -> Option<u64> {
    nanoprobe_transport::names::parse(name).map(|p| p.entity)
}

/// `menu(entity, target, scope)`: queue a `MENU` record for a selected item,
/// with player 0 (spec/records.md, "Layout: MENU"). `entity` is the item's
/// `EntityId` as decimal text. `target` is a coalition number, a group's
/// platform name, or nothing; the record holds the number or the group's
/// `EntityId`, and 0 for nothing. Never raises.
unsafe fn work_menu(state: *mut State) -> c_int {
    // SAFETY: a live state; every read is of an argument and never raises.
    let (entity, target, scope) = unsafe {
        let entity = arg_text(state, 1).parse::<u64>().ok();
        let target = if lua::lua_type(state, 2) == lua::TNUMBER {
            arg_id(state, 2).unwrap_or(0)
        } else {
            entity_of_name(&arg_text(state, 2)).unwrap_or(0)
        };
        (entity, target, arg_u32(state, 3))
    };
    if let Some(entity) = entity {
        driver::menu(&Menu {
            entity,
            target,
            scope,
            player: 0,
        });
    }
    0
}

/// `extension(name, calls, total_us, max_us, rev, skipped, stopped)`: write
/// one `EXTENSION` record in this frame. The agent's own applied revision of
/// the extension replaces `rev`. `stopped` is a boolean or a number. Returns
/// whether the record was written. Never raises.
unsafe fn work_extension(state: *mut State) -> c_int {
    // SAFETY: a live state; every read is of an argument and never raises.
    let stats = unsafe {
        ExtensionStats {
            name: arg_text(state, 1),
            calls: arg_count(state, 2),
            total_us: arg_count(state, 3),
            max_us: arg_count(state, 4),
            rev: arg_count(state, 5),
            events_skipped: arg_count(state, 6),
            stopped: u32::from(match lua::lua_type(state, 7) {
                lua::TBOOLEAN => lua::lua_toboolean(state, 7) != 0,
                lua::TNUMBER => arg_u32(state, 7) != 0,
                _ => false,
            }),
        }
    };
    let written = driver::extension(stats);
    // SAFETY: a live state with a free slot.
    unsafe { push_bool(state, written) }
}

/// `extension_budget([spent_us[, active]])`: charge `spent_us` microseconds
/// of extension hook time to the frame's commanded work. A boolean `active`
/// says whether an extension has work for the next pump: the agent then asks
/// for a pump on every frame of a running mission. Returns the microseconds
/// of the budget left, the active campaign as decimal text, whether raw Lua
/// is on, and the lifecycle code (3 running, 4 paused). Never raises.
unsafe fn work_extension_budget(state: *mut State) -> c_int {
    // SAFETY: a live state; reading an argument never raises.
    let (spent, active) = unsafe {
        let active =
            (lua::lua_type(state, 2) == lua::TBOOLEAN).then(|| lua::lua_toboolean(state, 2) != 0);
        (arg_num(state, 1, 0.0), active)
    };
    let budget = driver::extension_budget(spent, active);
    // The campaign's decimal text, at most 20 bytes, goes to a stack buffer
    // and the budget is dropped before any call that can raise: nothing with
    // a destructor is live across the pushes.
    let mut text = [0u8; 20];
    let n = budget.campaign.len().min(text.len());
    text[..n].copy_from_slice(&budget.campaign.as_bytes()[..n]);
    let (left_us, raw_lua, lifecycle) = (budget.left_us, budget.raw_lua, budget.lifecycle);
    drop(budget);
    // SAFETY: a live state with four free slots.
    unsafe {
        lua::lua_pushnumber(state, left_us);
        lua::lua_pushlstring(state, text.as_ptr().cast(), n);
        lua::lua_pushboolean(state, c_int::from(raw_lua));
        lua::lua_pushnumber(state, f64::from(lifecycle));
    }
    4
}

/// What `emit` read from its arguments, for [`work_emit`]: the object IDs and
/// the payload, or the reason it refuses.
static EMIT: Mutex<Option<EmitArgs>> = Mutex::new(None);

/// The object IDs and the payload of an `emit`, or the reason it refuses.
type EmitArgs = Result<(Vec<u64>, Lv), &'static str>;

/// `emit(name, payload, objects)`: queue an `AUTHOR_EVENT` record
/// (spec/commands.md, "emit"). Returns true, or false and a reason: `name`,
/// `too_large`, `too_deep`, `objects` or `disabled`. The arguments are read
/// under `lua_pcall` with raw table access only. Never raises.
unsafe fn work_emit(state: *mut State) -> c_int {
    // SAFETY: a live state from the entry.
    let outcome = unsafe { emit_outcome(state) };
    // SAFETY: a live state with two free slots.
    unsafe {
        lua::lua_settop(state, 0);
        match outcome {
            Ok(()) => push_bool(state, true),
            Err(reason) => {
                lua::lua_pushboolean(state, 0);
                lua::push_str(state, reason);
                2
            }
        }
    }
}

/// Run `emit`: the checks in the order of the spec's reasons, then the read,
/// then the record.
unsafe fn emit_outcome(state: *mut State) -> Result<(), &'static str> {
    if !driver::raw_lua() {
        return Err("disabled");
    }
    // SAFETY: a live state; reading a value never raises. The checked value
    // is dropped here, so nothing owned is live across the calls below that
    // can raise.
    let valid = matches!(
        unsafe { scalar(state, 1) },
        Lv::String(s) if (1..=driver::EMIT_NAME_MAX).contains(&s.len())
    );
    if !valid {
        return Err("name");
    }
    *EMIT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    // SAFETY: a live state; the closure runs under `lua_pcall` on copies of
    // the payload and the objects, and the stack has the free slots Lua
    // guarantees a C function.
    let failed = unsafe {
        lua::lua_settop(state, 3);
        lua::lua_pushcclosure(state, read_emit, 0);
        lua::lua_pushvalue(state, 2);
        lua::lua_pushvalue(state, 3);
        lua::lua_pcall(state, 2, 0, 0) != 0
    };
    // The name is read again, now that no call that can raise follows.
    // SAFETY: a live state; the name is still argument 1.
    let name = match unsafe { scalar(state, 1) } {
        Lv::String(s) => String::from_utf8_lossy(&s).into_owned(),
        _ => String::new(),
    };
    // SAFETY: a live state.
    unsafe { lua::lua_settop(state, 0) };
    let read = EMIT.lock().unwrap_or_else(|e| e.into_inner()).take();
    match read {
        Some(Ok((objects, payload))) if !failed => driver::emit(&name, &objects, payload),
        Some(Err(reason)) => Err(reason),
        _ => Err("too_large"),
    }
}

/// Read `emit`'s payload (argument 1) and objects (argument 2) into [`EMIT`].
/// Called only under `lua_pcall`. The two calls that can allocate in Lua, the
/// stack grow and the push of the key `id_`, come first, before this
/// function owns a value; the raw reads after them allocate nothing in Lua,
/// so no raise crosses a frame that owns a value.
#[inline(never)]
unsafe extern "C" fn read_emit(state: *mut State) -> c_int {
    // SAFETY: a live state. 3 slots per table level for 16 levels, and the
    // arguments.
    if unsafe { lua::lua_checkstack(state, 80) } == 0 {
        *EMIT.lock().unwrap_or_else(|e| e.into_inner()) = Some(Err("too_large"));
        return 0;
    }
    // SAFETY: as above; the key sits at index 3 for the reads below.
    unsafe { lua::push_str(state, "id_") };
    // SAFETY: the reads below are raw and allocate nothing in Lua.
    let read = unsafe {
        read_objects(state, 2, 3).and_then(|objects| {
            let mut rs = EmitRead {
                bytes: 0,
                path: Vec::new(),
            };
            read_payload(state, 1, 0, &mut rs).map(|payload| (objects, payload))
        })
    };
    *EMIT.lock().unwrap_or_else(|e| e.into_inner()) = Some(read);
    0
}

/// The object IDs of `emit`'s `objects` at `index`: nothing, or an array of
/// at most 16 entries, each a unit object or the number in its `id_`. `key`
/// is the index of the string `id_`.
unsafe fn read_objects(
    state: *mut State,
    index: c_int,
    key: c_int,
) -> Result<Vec<u64>, &'static str> {
    // SAFETY: a live state with the stack grown by the caller; `lua_type`,
    // `lua_objlen`, `lua_next` and the raw reads never raise.
    unsafe {
        match lua::lua_type(state, index) {
            lua::TNIL | lua::TNONE => return Ok(Vec::new()),
            lua::TTABLE => {}
            _ => return Err("objects"),
        }
        let len = lua::lua_objlen(state, index);
        if len > driver::EMIT_OBJECTS_MAX {
            return Err("objects");
        }
        // An array: the table holds no key past its length.
        let mut pairs = 0;
        lua::lua_pushnil(state);
        while lua::lua_next(state, index) != 0 {
            pairs += 1;
            lua::lua_settop(state, -2);
            if pairs > len {
                lua::lua_settop(state, -2);
                return Err("objects");
            }
        }
        let mut out = Vec::with_capacity(len);
        for i in 1..=len {
            lua::lua_rawgeti(state, index, i as c_int);
            let id = match lua::lua_type(state, -1) {
                lua::TNUMBER => arg_id(state, -1),
                lua::TTABLE => {
                    lua::lua_pushvalue(state, key);
                    lua::lua_rawget(state, -2);
                    let id = arg_id(state, -1);
                    lua::lua_settop(state, -2);
                    id
                }
                _ => None,
            };
            lua::lua_settop(state, -2);
            match id {
                Some(id) => out.push(id),
                None => return Err("objects"),
            }
        }
        Ok(out)
    }
}

/// What [`read_payload`] has read: the bytes the value encodes to so far, and
/// the tables on its path.
struct EmitRead {
    bytes: usize,
    path: Vec<*const core::ffi::c_void>,
}

impl EmitRead {
    /// Count `n` more encoded bytes; false past the payload limit.
    fn take(&mut self, n: usize) -> Result<(), &'static str> {
        self.bytes = self.bytes.saturating_add(n);
        if self.bytes > driver::EMIT_PAYLOAD_MAX {
            Err("too_large")
        } else {
            Ok(())
        }
    }
}

/// The value at `index` as an `lv`, read with raw access: a table by its
/// pairs, a function, userdata or thread by its type name. Tables nest 16
/// deep at most, as the codec carries them: a deeper table, or one inside
/// itself, is `too_deep`. A value that encodes to more than 16 KiB is
/// `too_large`, and the read stops there. Keys that are not strings, numbers
/// or booleans are left out.
///
/// # Safety
///
/// A live state with the stack grown for 16 levels, under `lua_pcall`. The
/// reads call no metamethod and allocate nothing in Lua.
unsafe fn read_payload(
    state: *mut State,
    index: c_int,
    depth: usize,
    rs: &mut EmitRead,
) -> Result<Lv, &'static str> {
    // SAFETY: the caller's contract.
    unsafe {
        if lua::lua_type(state, index) != lua::TTABLE {
            let v = scalar(state, index);
            let n = match &v {
                Lv::Nil | Lv::Bool(_) => 1,
                Lv::Number(_) => 9,
                Lv::String(s) => 5 + s.len(),
                Lv::Other(name) => 3 + name.len(),
                Lv::Table(_) => unreachable!("a table is read below"),
            };
            rs.take(n)?;
            return Ok(v);
        }
        if depth >= nanoprobe_transport::codec::MAX_DEPTH {
            return Err("too_deep");
        }
        rs.take(5)?;
        let base = lua::lua_gettop(state);
        let at = if index < 0 { base + index + 1 } else { index };
        let id = lua::lua_topointer(state, at);
        if rs.path.contains(&id) {
            return Err("too_deep");
        }
        rs.path.push(id);
        let mut pairs = Vec::new();
        lua::lua_pushnil(state);
        while lua::lua_next(state, at) != 0 {
            let top = lua::lua_gettop(state);
            let key = match lua::lua_type(state, top - 1) {
                lua::TSTRING | lua::TNUMBER | lua::TBOOLEAN => Some(scalar(state, top - 1)),
                _ => None,
            };
            if let Some(key) = key {
                let before = rs.bytes;
                let kn = match &key {
                    Lv::Bool(_) => 1,
                    Lv::Number(_) => 9,
                    Lv::String(s) => 5 + s.len(),
                    _ => 0,
                };
                if let Err(e) = rs.take(kn) {
                    lua::lua_settop(state, base);
                    return Err(e);
                }
                match read_payload(state, top, depth + 1, rs) {
                    Ok(v) => pairs.push((key, v)),
                    Err(e) => {
                        lua::lua_settop(state, base);
                        return Err(e);
                    }
                }
                debug_assert!(rs.bytes >= before);
            }
            lua::lua_settop(state, top - 1);
        }
        lua::lua_settop(state, base);
        rs.path.pop();
        Ok(Lv::Table(pairs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_platform_name_ends_in_its_entity() {
        assert_eq!(entity_of_name("np!enPb"), Some(37));
        assert_eq!(entity_of_name("np.enPz"), Some(61));
        assert_eq!(entity_of_name("np.enP10"), Some(62));
        assert_eq!(entity_of_name("np!enP"), None);
        assert_eq!(entity_of_name("xx!enPb"), None);
        assert_eq!(entity_of_name("np!enP0"), None);
        assert_eq!(entity_of_name("np!enP-"), None);
        assert_eq!(entity_of_name(""), None);
    }
}
