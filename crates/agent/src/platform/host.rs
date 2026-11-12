//! The host-native side: macOS and Linux, for tests under a stock Lua 5.1.

use std::path::Path;

/// The calling thread's ID.
pub fn thread_id() -> u64 {
    // SAFETY: pthread_self has no preconditions.
    unsafe { libc::pthread_self() as usize as u64 }
}

/// The monotonic clock, in ticks of [`frequency`].
pub fn ticks() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid out-pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec * 1_000_000_000 + ts.tv_nsec
}

/// Ticks per second of [`ticks`].
pub fn frequency() -> i64 {
    1_000_000_000
}

/// Keep the module loaded until the process ends. The host build does not.
pub fn pin() -> bool {
    false
}

/// The module's file name, without its folder: the file the loader opened,
/// found from an address inside it, as the Windows build does.
pub fn module_name() -> String {
    let mut info = libc::Dl_info {
        dli_fname: core::ptr::null(),
        dli_fbase: core::ptr::null_mut(),
        dli_sname: core::ptr::null(),
        dli_saddr: core::ptr::null_mut(),
    };
    // SAFETY: the address is this function's, inside the module; `info` is a
    // valid out-pointer, and dladdr leaves it alone when it returns 0.
    let found = unsafe { libc::dladdr(module_name as *const libc::c_void, &mut info) };
    if found == 0 || info.dli_fname.is_null() {
        return "host".into();
    }
    // SAFETY: dladdr returned a NUL-terminated path owned by the loader.
    let path = unsafe { core::ffi::CStr::from_ptr(info.dli_fname) }.to_string_lossy();
    path.rsplit(['/', '\\']).next().unwrap_or("host").to_owned()
}

/// The PDB signature that matches this build. The host build has none.
pub fn pdb_id() -> String {
    String::new()
}

/// Record native exceptions to `path`. The host build does not.
pub fn install_crash_handler(_path: &Path) -> bool {
    false
}

/// A deliberate access violation. The host tests never call it.
pub fn fault() {
    // SAFETY: deliberately not safe; this is the fault the crash capture
    // test needs. A null write is a SIGSEGV on the host.
    unsafe { core::ptr::write_volatile(core::ptr::null_mut::<u64>(), 1) }
}
