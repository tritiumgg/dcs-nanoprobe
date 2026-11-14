//! The Lua 5.1 C API, as far as the agent uses it.
//!
//! DCS's Lua or the host's: `build.rs` and the `dcs-lua` feature decide which.
//! The API is stock Lua 5.1, so these are the stock declarations, and
//! `vendor/lua/lua.def` decides which of the 114 exports the agent may name.
//!
//! A Lua error is a `longjmp`. It must never pass through a Rust frame that owns
//! a value with a destructor: on Windows the unwind runs that destructor's
//! cleanup in the middle of Lua's error handling, and DCS exits. Entry points
//! own nothing and call an `#[inline(never)]` worker; anything that can raise
//! runs under `lua_pcall`. `tools/raiseshape.sh` checks the entry points.
//!
//! Of the calls below, `lua_error` always raises, `lua_getfield` can raise
//! through a metamethod, and the calls that allocate can raise when Lua is out
//! of memory. The agent calls `lua_getfield` only on the registry, which has no
//! metatable, and treats running out of memory inside DCS as fatal anyway.

use core::ffi::{c_char, c_int, c_void};

/// An opaque Lua state.
pub type State = c_void;

/// A C function Lua can call.
pub type CFunction = unsafe extern "C" fn(state: *mut State) -> c_int;

/// The pseudo-index of the registry in Lua 5.1.
pub const REGISTRYINDEX: c_int = -10000;

/// `lua_pcall`'s request for every result.
pub const MULTRET: c_int = -1;

/// The type tag `lua_type` returns for an index past the top.
pub const TNONE: c_int = -1;
/// nil.
pub const TNIL: c_int = 0;
/// A boolean.
pub const TBOOLEAN: c_int = 1;
/// The type tag `lua_type` returns for a number.
pub const TNUMBER: c_int = 3;
/// A string.
pub const TSTRING: c_int = 4;
/// A table.
pub const TTABLE: c_int = 5;
/// A function.
pub const TFUNCTION: c_int = 6;

unsafe extern "C" {
    /// Push a fresh table sized for `narr` array and `nrec` hash entries.
    pub unsafe fn lua_createtable(state: *mut State, narr: c_int, nrec: c_int);

    /// Push `n`. `lua_Integer` is `ptrdiff_t` in a stock 5.1 build.
    pub unsafe fn lua_pushinteger(state: *mut State, n: isize);

    /// Push the number `n`.
    pub unsafe fn lua_pushnumber(state: *mut State, n: f64);

    /// Push a boolean.
    pub unsafe fn lua_pushboolean(state: *mut State, b: c_int);

    /// Push nil.
    pub unsafe fn lua_pushnil(state: *mut State);

    /// Push the `len` bytes at `s` as a string, which Lua copies.
    pub unsafe fn lua_pushlstring(state: *mut State, s: *const c_char, len: usize);

    /// Push a C function with `n` upvalues taken from the stack.
    pub unsafe fn lua_pushcclosure(state: *mut State, f: CFunction, n: c_int);

    /// Push a copy of the value at `index`.
    pub unsafe fn lua_pushvalue(state: *mut State, index: c_int);

    /// Pop the top value into `key` of the table at `index`.
    pub unsafe fn lua_setfield(state: *mut State, index: c_int, key: *const c_char);

    /// Push `t[key]` for the table `t` at `index`. Can raise through a
    /// metamethod.
    pub unsafe fn lua_getfield(state: *mut State, index: c_int, key: *const c_char);

    /// Push `t[n]` for the table `t` at `index`, without metamethods.
    pub unsafe fn lua_rawgeti(state: *mut State, index: c_int, n: c_int);

    /// Replace the key on top of the stack with `t[key]` for the table `t`
    /// at `index`, without metamethods.
    pub unsafe fn lua_rawget(state: *mut State, index: c_int);

    /// Set `t[n]` for the table `t` at `index` to the top value and pop it,
    /// without metamethods. Allocates, and so can raise a memory error, only
    /// when the table must grow: callers write only slots that exist.
    pub unsafe fn lua_rawseti(state: *mut State, index: c_int, n: c_int);

    /// The index of the top of the stack, which is the number of values on it.
    pub unsafe fn lua_gettop(state: *mut State) -> c_int;

    /// Set the top of the stack to `index`, popping or pushing nils.
    pub unsafe fn lua_settop(state: *mut State, index: c_int);

    /// Move the top value to `index`, shifting the values above it up.
    pub unsafe fn lua_insert(state: *mut State, index: c_int);

    /// The type tag of the value at `index`, or -1 for an index past the top.
    pub unsafe fn lua_type(state: *mut State, index: c_int) -> c_int;

    /// Whether the value at `index` is neither nil nor false.
    pub unsafe fn lua_toboolean(state: *mut State, index: c_int) -> c_int;

    /// The name of a type tag. A static string; never raises.
    pub unsafe fn lua_typename(state: *mut State, tag: c_int) -> *const c_char;

    /// The value at `index` as a number, or 0 when it is not one.
    pub unsafe fn lua_tonumber(state: *mut State, index: c_int) -> f64;

    /// The value at `index` as a string, converting a number in place, or null.
    pub unsafe fn lua_tolstring(state: *mut State, index: c_int, len: *mut usize) -> *const c_char;

    /// The length of the string or table at `index`, by the primitive rule.
    pub unsafe fn lua_objlen(state: *mut State, index: c_int) -> usize;

    /// Call the function below `nargs` arguments in protected mode.
    pub unsafe fn lua_pcall(
        state: *mut State,
        nargs: c_int,
        nresults: c_int,
        errfunc: c_int,
    ) -> c_int;

    /// Raise the value on top of the stack as an error. Never returns.
    pub unsafe fn lua_error(state: *mut State) -> c_int;
}

/// Push a Rust string, which Lua copies.
///
/// # Safety
///
/// `state` must be live with a free stack slot.
pub unsafe fn push_str(state: *mut State, s: &str) {
    // SAFETY: the caller's guarantees; Lua copies the bytes.
    unsafe { lua_pushlstring(state, s.as_ptr().cast::<c_char>(), s.len()) }
}

/// The string at `index`, copied, or None when the value is not a string or
/// a number.
///
/// # Safety
///
/// `state` must be live and `index` valid.
pub unsafe fn to_string(state: *mut State, index: c_int) -> Option<String> {
    // SAFETY: the caller guarantees a live state and a valid index.
    let t = unsafe { lua_type(state, index) };
    if t != TSTRING && t != TNUMBER {
        return None;
    }
    let mut len = 0;
    // SAFETY: the value is a string or a number, so Lua returns a pointer to
    // `len` bytes that live while the value is on the stack.
    let p = unsafe { lua_tolstring(state, index, &mut len) };
    if p.is_null() {
        return None;
    }
    // SAFETY: as above.
    let bytes = unsafe { core::slice::from_raw_parts(p.cast::<u8>(), len) };
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// The name of a type tag, copied.
///
/// # Safety
///
/// `state` must be live.
pub unsafe fn type_name(state: *mut State, tag: c_int) -> String {
    // SAFETY: the caller guarantees a live state; Lua returns a static
    // NUL-terminated string for any tag.
    let p = unsafe { lua_typename(state, tag) };
    if p.is_null() {
        return String::new();
    }
    // SAFETY: as above.
    unsafe { core::ffi::CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned()
}
