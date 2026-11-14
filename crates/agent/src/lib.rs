//! The agent: the DLL DCS loads, and its one entry point, `luaopen_nanoprobe`.
//!
//! DCS opens the agent by explicit path, because `package.cpath` is not set in
//! the state that loads it:
//!
//! ```lua
//! local loader = assert(package.loadlib(path, 'luaopen_nanoprobe'))
//! local agent  = loader()
//! ```
//!
//! The same call opens the host-native `.so` or `.dylib` under a stock Lua 5.1,
//! which is how the agent is tested with no DCS present.

#[cfg(any(unix, feature = "dcs-lua"))]
mod driver;
#[cfg(any(unix, feature = "dcs-lua"))]
mod entries;
#[cfg(any(unix, feature = "dcs-lua"))]
mod lua;
#[cfg(any(unix, feature = "dcs-lua"))]
mod platform;
#[cfg(any(unix, feature = "dcs-lua"))]
mod state;

/// The agent's release version, the workspace version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The version of the interface between the agent and its Lua shim. It moves
/// when a call on the table `luaopen_nanoprobe` returns is added, removed, or
/// changed in a way an older caller would misread.
pub const ABI_VERSION: &str = "7";

/// What tools outside DCS find the ABI version by in the DLL's bytes. The
/// version alone is too short to search for.
const MARKER_PREFIX: &[u8] = b"nanoprobe-abi=";

/// The prefix, the version, and a NUL, since whatever the linker places after
/// the marker can be printable.
const MARKER_LEN: usize = MARKER_PREFIX.len() + ABI_VERSION.len() + 1;

/// The marker, built from [`ABI_VERSION`] so the two cannot drift. The open
/// pushes the ABI version from these bytes, so the linker keeps them.
#[used]
static ABI_MARKER: [u8; MARKER_LEN] = marker();

const fn marker() -> [u8; MARKER_LEN] {
    let version = ABI_VERSION.as_bytes();
    let mut out = [0; MARKER_LEN];
    let mut i = 0;
    while i < MARKER_PREFIX.len() {
        out[i] = MARKER_PREFIX[i];
        i += 1;
    }
    let mut j = 0;
    while j < version.len() {
        out[i + j] = version[j];
        j += 1;
    }
    out
}

/// How many Lua states in this process have opened the agent. A state that
/// opens it again gets its first table back and is not counted.
#[cfg(any(unix, feature = "dcs-lua"))]
static OPENS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// How many Lua states in this process have opened the agent.
#[cfg(any(unix, feature = "dcs-lua"))]
pub(crate) fn opens() -> usize {
    OPENS.load(core::sync::atomic::Ordering::Relaxed)
}

/// The registry key under which a state keeps its agent table.
#[cfg(any(unix, feature = "dcs-lua"))]
const REGISTRY_KEY: &core::ffi::CStr = c"nanoprobe.agent";

/// Open the agent in `state`, leaving one table on the stack: `version`,
/// `abi`, `build`, `opens` (the number of states that had opened the agent when
/// this one did), and the functions in [`entries::ENTRIES`].
///
/// Opening is idempotent: a state that opens the agent again gets the same
/// table back. The first open in the process pins the module, installs the
/// panic hook, and counts nothing else.
///
/// This is an entry point, so it owns nothing and catches every panic. On a
/// panic it leaves nothing on the stack, and the loader returns nil.
///
/// # Safety
///
/// `state` must be a live `lua_State` from the Lua that opened this module, with
/// room for two stack slots. Lua guarantees both when it calls an opener.
#[cfg(any(unix, feature = "dcs-lua"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaopen_nanoprobe(state: *mut core::ffi::c_void) -> core::ffi::c_int {
    // SAFETY: the caller's guarantees pass through to `open` unchanged.
    std::panic::catch_unwind(|| unsafe { open(state) }).unwrap_or(0)
}

/// The work behind [`luaopen_nanoprobe`], in its own frame.
///
/// # Safety
///
/// As for [`luaopen_nanoprobe`].
#[cfg(any(unix, feature = "dcs-lua"))]
#[inline(never)]
unsafe fn open(state: *mut core::ffi::c_void) -> core::ffi::c_int {
    use core::ffi::c_char;

    state::once();

    // SAFETY: the caller guarantees a live lua_State with two free slots.
    // The registry has no metatable, so lua_getfield cannot raise through
    // one. Lua copies the bytes it is given, so nothing this crate allocated
    // is left for DCS's C runtime to free.
    unsafe {
        lua::lua_getfield(state, lua::REGISTRYINDEX, REGISTRY_KEY.as_ptr());
        if lua::lua_type(state, -1) == lua::TTABLE {
            return 1;
        }
        lua::lua_settop(state, -2);

        let opens = OPENS.fetch_add(1, core::sync::atomic::Ordering::Relaxed) + 1;
        let abi = &ABI_MARKER[MARKER_PREFIX.len()..MARKER_LEN - 1];

        lua::lua_createtable(state, 0, 4 + entries::ENTRIES.len() as core::ffi::c_int);

        lua::lua_pushlstring(state, VERSION.as_ptr().cast::<c_char>(), VERSION.len());
        lua::lua_setfield(state, -2, c"version".as_ptr());

        lua::lua_pushlstring(state, abi.as_ptr().cast::<c_char>(), abi.len());
        lua::lua_setfield(state, -2, c"abi".as_ptr());

        lua::push_str(state, state::build());
        lua::lua_setfield(state, -2, c"build".as_ptr());

        // A count that outgrew isize would need more Lua states than exist.
        lua::lua_pushinteger(state, opens as isize);
        lua::lua_setfield(state, -2, c"opens".as_ptr());

        for (name, function) in entries::ENTRIES {
            lua::lua_pushcclosure(state, *function, 0);
            lua::lua_setfield(state, -2, name.as_ptr());
        }

        lua::lua_pushvalue(state, -1);
        lua::lua_setfield(state, lua::REGISTRYINDEX, REGISTRY_KEY.as_ptr());
    }

    1
}
