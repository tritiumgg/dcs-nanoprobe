//! Free space on the file system that holds a path (spec/persistence.md,
//! "Free space").

use std::io;
use std::path::Path;

/// Bytes a process without special rights may still write on the file
/// system that holds `path`.
#[cfg(unix)]
pub fn free_bytes(path: &Path) -> io::Result<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a path with a NUL byte"))?;
    // SAFETY: statvfs is plain data; all zero bytes is a valid value.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a NUL-terminated path and `s` a statvfs the call fills
    // in; nothing outlives the call.
    let r = unsafe { libc::statvfs(c.as_ptr(), &mut s) };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    #[allow(clippy::useless_conversion)]
    Ok(u64::from(s.f_bavail).saturating_mul(u64::from(s.f_frsize)))
}

/// Bytes the caller may still write on the volume that holds `path`.
#[cfg(windows)]
pub fn free_bytes(path: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut free = 0u64;
    // SAFETY: `wide` is a NUL-terminated UTF-16 path; the out pointers are
    // valid for the call or null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(free)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_temp_folder_has_free_space_and_a_missing_one_is_an_error() {
        let dir = tempfile::tempdir().expect("dir");
        assert!(super::free_bytes(dir.path()).expect("free") > 0);
        assert!(super::free_bytes(&dir.path().join("missing")).is_err());
    }
}
