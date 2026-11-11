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

use nanoprobe_transport::log::{Level, Value};

use crate::lua::{self, State};
use crate::{platform, state};

/// What a worker returns to ask its entry to raise the value on top of the
/// stack.
const RAISE: c_int = -1;

/// Run `work` for an entry: count the call, catch a panic, and honour the
/// disabled state unless `always`. Returns the number of results, or
/// [`RAISE`].
#[inline(never)]
unsafe fn guarded(
    state: *mut State,
    name: &'static str,
    always: bool,
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
        Ok(n) => n,
        Err(_) => {
            state::trip(name);
            0
        }
    }
}

/// Define the entries: one `extern "C"` function per worker, and the list
/// the opener registers.
macro_rules! entries {
    ($($name:ident => $work:ident $(, $always:ident)?;)*) => {
        $(
            /// An entry point; see the module documentation.
            ///
            /// # Safety
            ///
            /// Called by Lua with a live state.
            unsafe extern "C" fn $name(state: *mut State) -> c_int {
                // SAFETY: Lua passes a live state.
                let n = unsafe { guarded(state, stringify!($name), entries!(@always $($always)?), $work) };
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
    (@always always) => { true };
    (@always) => { false };
}

entries! {
    status => work_status, always;
    configure => work_configure;
    log => work_log;
    thread => work_thread;
    clock => work_clock;
    qpc_cost => work_qpc_cost;
    crumb => work_crumb;
    raise => work_raise;
    call => work_call;
    panic => work_panic;
    fault => work_fault;
    sink => work_sink;
    sink_take => work_sink_take;
    csweep => work_csweep;
    sock_open => work_sock_open;
    sock_frame => work_sock_frame;
    sock_close => work_sock_close;
    map_open => work_map_open;
    map_frame => work_map_frame;
    map_load => work_map_load;
    map_close => work_map_close;
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
    // SAFETY: a live state; the table takes one slot, each field one more.
    unsafe {
        lua::lua_createtable(state, 0, 10);
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

/// `log(level, message)`: one record in the agent's log.
unsafe fn work_log(state: *mut State) -> c_int {
    // SAFETY: a live state.
    let level = unsafe { lua::to_string(state, 1) }.unwrap_or_default();
    // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
    let message = unsafe { lua::to_string(state, 2) }.unwrap_or_default();
    let level = match level.as_str() {
        "error" => Level::Error,
        "warn" => Level::Warn,
        "debug" => Level::Debug,
        _ => Level::Info,
    };
    state::log(level, &message, &[]);
    0
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

/// The mapped file for the E24 probe.
static MAPPING: Mutex<Option<platform::Mapping>> = Mutex::new(None);
/// Words in the probe's mapping: 0 magic, 1 sequence, 2..10 the payload the
/// agent writes, 16 the peer's echo, 17 the peer's sequence.
const MAP_WORDS: usize = 64;
const MAP_MAGIC: u64 = 0x6e70_6d61_7070_6564;

/// `map_open(path)`: map the file at `path`, 512 bytes, and write the magic.
unsafe fn work_map_open(state: *mut State) -> c_int {
    // SAFETY: a live state.
    let Some(path) = (unsafe { lua::to_string(state, 1) }) else {
        // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
        return unsafe { fail(state, "map_open needs a path") };
    };
    match platform::Mapping::open(&PathBuf::from(&path), MAP_WORDS * 8) {
        Ok(mapping) => {
            let words = mapping.words();
            words[1].store(0, core::sync::atomic::Ordering::Release);
            words[0].store(MAP_MAGIC, core::sync::atomic::Ordering::Release);
            if let Ok(mut slot) = MAPPING.try_lock() {
                *slot = Some(mapping);
            }
            // SAFETY: a live state with a free slot.
            unsafe { lua::lua_pushboolean(state, 1) };
            1
        }
        // SAFETY: as above.
        Err(e) => unsafe { fail(state, &e) },
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
    let Some(mapping) = slot.as_ref() else {
        // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
        return unsafe { fail(state, "no mapping; call map_open") };
    };
    let words = mapping.words();
    let seq = words[1].load(Acquire);
    words[1].store(seq + 1, Release);
    for word in &words[2..10] {
        word.store(value, Release);
    }
    words[1].store(seq + 2, Release);
    let echo = words[16].load(Acquire);
    let peer = words[17].load(Acquire);
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
    let index = unsafe { arg_num(state, 1, 0.0) } as usize;
    let Ok(slot) = MAPPING.try_lock() else {
        // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
        return unsafe { fail(state, "busy") };
    };
    let Some(word) = slot.as_ref().and_then(|m| m.words().get(index)) else {
        // SAFETY: a live state from the entry, with the free slots Lua guarantees a C function.
        return unsafe { fail(state, "no mapping, or index out of range") };
    };
    let value = word.load(core::sync::atomic::Ordering::Acquire);
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
