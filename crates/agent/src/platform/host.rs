//! The host-native side: macOS and Linux, for tests under a stock Lua 5.1.

use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::atomic::AtomicU64;

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

/// The module's file name, without its folder.
pub fn module_name() -> String {
    "host".into()
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

/// A file mapped into memory, shared with every other process that maps it.
pub struct Mapping {
    ptr: *mut u8,
    len: usize,
}

// SAFETY: the mapping is plain shared memory; access goes through atomics.
unsafe impl Send for Mapping {}

impl Mapping {
    /// Map `path`, creating it and growing it to `len` bytes as needed.
    pub fn open(path: &Path, len: usize) -> Result<Mapping, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let size = file.metadata().map_err(|e| e.to_string())?.len();
        if size < len as u64 {
            file.set_len(len as u64).map_err(|e| e.to_string())?;
        }
        // SAFETY: a fresh shared mapping of an open file of at least `len`
        // bytes; the file may close afterwards, the mapping stays.
        let ptr = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(format!(
                "mmap {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        Ok(Mapping {
            ptr: ptr.cast(),
            len,
        })
    }

    /// The mapping as 64-bit words.
    pub fn words(&self) -> &[AtomicU64] {
        // SAFETY: the mapping is page-aligned, `len` bytes long and lives as
        // long as `self`; AtomicU64 has the layout of u64.
        unsafe { core::slice::from_raw_parts(self.ptr.cast::<AtomicU64>(), self.len / 8) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly what `open` mapped.
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
    }
}
