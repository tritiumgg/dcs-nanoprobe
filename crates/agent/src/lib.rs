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
mod lua;

/// The agent's release version, the workspace version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The version of the interface between the agent and its Lua shim. It moves
/// when a call on the table `luaopen_nanoprobe` returns is added, removed, or
/// changed in a way an older caller would misread.
pub const ABI_VERSION: &str = "0";

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

/// How many times this process has opened the agent. Every Lua state that
/// loads the DLL shares it.
#[cfg(any(unix, feature = "dcs-lua"))]
static OPENS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Open the agent in `state`, leaving one table on the stack: `version`, `abi`,
/// and `opens`, the number of opens in this process so far.
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

    let opens = OPENS.fetch_add(1, core::sync::atomic::Ordering::Relaxed) + 1;
    let abi = &ABI_MARKER[MARKER_PREFIX.len()..MARKER_LEN - 1];

    // SAFETY: the caller guarantees a live lua_State with two free slots. The
    // table takes one and each pushed value the other; lua_setfield pops the
    // value. Lua copies the bytes it is given, so nothing this crate allocated
    // is left for DCS's C runtime to free.
    unsafe {
        lua::lua_createtable(state, 0, 3);

        lua::lua_pushlstring(state, VERSION.as_ptr().cast::<c_char>(), VERSION.len());
        lua::lua_setfield(state, -2, c"version".as_ptr());

        lua::lua_pushlstring(state, abi.as_ptr().cast::<c_char>(), abi.len());
        lua::lua_setfield(state, -2, c"abi".as_ptr());

        // A count that outgrew isize would need more Lua states than exist.
        lua::lua_pushinteger(state, opens as isize);
        lua::lua_setfield(state, -2, c"opens".as_ptr());
    }

    1
}
