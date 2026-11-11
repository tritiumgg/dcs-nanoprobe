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
//! runs under `lua_pcall`.

use core::ffi::{c_char, c_int, c_void};

unsafe extern "C" {
    /// Push a fresh table sized for `narr` array and `nrec` hash entries.
    pub unsafe fn lua_createtable(state: *mut c_void, narr: c_int, nrec: c_int);

    /// Push `n`. `lua_Integer` is `ptrdiff_t` in a stock 5.1 build.
    pub unsafe fn lua_pushinteger(state: *mut c_void, n: isize);

    /// Push the `len` bytes at `s` as a string, which Lua copies.
    pub unsafe fn lua_pushlstring(state: *mut c_void, s: *const c_char, len: usize);

    /// Pop the top value into `key` of the table at `index`.
    pub unsafe fn lua_setfield(state: *mut c_void, index: c_int, key: *const c_char);
}
