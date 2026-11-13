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
use nanoprobe_transport::log::{Level, Value};
use nanoprobe_transport::map::MappedFile;
use nanoprobe_transport::mem::Memory;

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
    lifecycle => work_lifecycle;
    log => work_log, hot;
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
        lua::lua_createtable(state, 0, 17);
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

/// `frame(real, model, paused)`: the frame's pump. Returns 0, 1 when the
/// hook should reload the shim, 2 when the agent is disabled and the hook
/// stops calling, or 3 when no link is open.
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
    let code = driver::frame(real, model, paused);
    // SAFETY: a live state with a free slot.
    unsafe { lua::lua_pushinteger(state, code as isize) };
    1
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

/// `csweep(f, list, nres)`: the C-driven shape (E5). For each value in
/// `list`, call `f(value)` under `lua_pcall`, read `nres` numbers, and add
/// them into the sink. Returns the rows done and the rows whose call raised.
unsafe fn work_csweep(state: *mut State) -> c_int {
    // SAFETY: a live state; every call that can raise runs under lua_pcall,
    // and the stack is reset to `base` after each row.
    unsafe {
        if lua::lua_type(state, 1) != lua::TFUNCTION || lua::lua_type(state, 2) != lua::TTABLE {
            return raise_with(state, "csweep needs a function and a list");
        }
        let nres = arg_num(state, 3, 1.0).clamp(0.0, 8.0) as c_int;
        let n = lua::lua_objlen(state, 2);
        let base = lua::lua_gettop(state);
        let (mut rows, mut errors, mut sum) = (0u64, 0u64, 0.0);
        for i in 1..=n {
            state::crumb(i as u64);
            lua::lua_pushvalue(state, 1);
            lua::lua_rawgeti(state, 2, i as c_int);
            if lua::lua_pcall(state, 1, nres, 0) == 0 {
                for k in 1..=nres {
                    sum += lua::lua_tonumber(state, -k);
                }
            } else {
                errors += 1;
            }
            lua::lua_settop(state, base);
            rows += 1;
        }
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
