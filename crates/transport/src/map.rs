//! The mapped file (spec/transport.md, "The file").

use std::fs::{File, OpenOptions};
use std::path::Path;
use std::sync::atomic::Ordering;

use crate::mem::{Memory, Raw};

/// A file mapped into memory, shared with every other process that maps it.
pub struct MappedFile {
    raw: Raw,
    writable: bool,
    file_len: u64,
    os: os::Map,
}

impl MappedFile {
    /// The agent's open: create the file or open it as it is, grow it to `len`
    /// bytes if it is smaller, never shrink it, and map the first `len` bytes.
    ///
    /// Only the agent's account and the sidecar's may open the file
    /// (spec/transport.md, "The file"). On Unix a new file gets mode 0660, for
    /// a sidecar that shares the agent's group. On Windows and under Wine the
    /// folder's permissions decide; the installer sets them.
    pub fn create(path: &Path, len: u64) -> std::io::Result<MappedFile> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o660);
        let file = options.open(path)?;
        let size = file.metadata()?.len();
        if size < len {
            file.set_len(len)?;
        }
        MappedFile::map(&file, len, size.max(len), true)
    }

    /// The sidecar's open: map the whole existing file for reading and writing.
    pub fn open(path: &Path) -> std::io::Result<MappedFile> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let len = file.metadata()?.len();
        MappedFile::map(&file, len, len, true)
    }

    /// The inspector's open: map the whole existing file for reading only. A
    /// store through it panics.
    pub fn open_read_only(path: &Path) -> std::io::Result<MappedFile> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        MappedFile::map(&file, len, len, false)
    }

    fn map(file: &File, len: u64, file_len: u64, writable: bool) -> std::io::Result<MappedFile> {
        if len < crate::layout::status::SIZE as u64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("the file holds {len} bytes, less than a status block"),
            ));
        }
        let len = usize::try_from(len).map_err(std::io::Error::other)?;
        let os = os::Map::new(file, len, writable)?;
        // SAFETY: the mapping is page-aligned, `len` bytes long, and lives as
        // long as `os`, which the `MappedFile` owns. No reference to it is made.
        let raw = unsafe { Raw::new(os.ptr(), len) };
        Ok(MappedFile {
            raw,
            writable,
            file_len,
            os,
        })
    }

    /// The file's length when it was mapped.
    pub fn file_len(&self) -> u64 {
        self.file_len
    }

    /// Whether the mapping may be written.
    pub fn writable(&self) -> bool {
        self.writable
    }

    fn writing(&self) {
        assert!(self.writable, "a store to a read-only mapping");
    }
}

impl Drop for MappedFile {
    fn drop(&mut self) {
        self.os.unmap();
    }
}

impl Memory for MappedFile {
    fn len(&self) -> usize {
        self.raw.len()
    }
    fn load(&self, off: usize, order: Ordering) -> u64 {
        self.raw.load(off, order)
    }
    fn store(&self, off: usize, value: u64, order: Ordering) {
        self.writing();
        self.raw.store(off, value, order)
    }
    fn load32(&self, off: usize, order: Ordering) -> u32 {
        self.raw.load32(off, order)
    }
    fn store32(&self, off: usize, value: u32, order: Ordering) {
        self.writing();
        self.raw.store32(off, value, order)
    }
    fn fence(&self, order: Ordering) {
        self.raw.fence(order)
    }
    fn read(&self, off: usize, out: &mut [u8]) {
        self.raw.read(off, out)
    }
    fn write(&self, off: usize, data: &[u8]) {
        self.writing();
        self.raw.write(off, data)
    }
    fn zero(&self, off: usize, n: usize) {
        self.writing();
        self.raw.zero(off, n)
    }
}

#[cfg(unix)]
mod os {
    use std::fs::File;
    use std::os::fd::AsRawFd;

    pub struct Map {
        ptr: *mut u8,
        len: usize,
    }

    // SAFETY: the mapping is shared memory owned by this value; every access
    // to it goes through `Raw`'s atomics and raw copies.
    unsafe impl Send for Map {}
    // SAFETY: as above.
    unsafe impl Sync for Map {}

    impl Map {
        pub fn new(file: &File, len: usize, writable: bool) -> std::io::Result<Map> {
            let prot = if writable {
                libc::PROT_READ | libc::PROT_WRITE
            } else {
                libc::PROT_READ
            };
            // SAFETY: a fresh shared mapping of an open file of at least `len`
            // bytes; the mapping outlives the file descriptor.
            let ptr = unsafe {
                libc::mmap(
                    core::ptr::null_mut(),
                    len,
                    prot,
                    libc::MAP_SHARED,
                    file.as_raw_fd(),
                    0,
                )
            };
            if ptr == libc::MAP_FAILED {
                return Err(std::io::Error::last_os_error());
            }
            Ok(Map {
                ptr: ptr.cast(),
                len,
            })
        }

        pub fn ptr(&self) -> *mut u8 {
            self.ptr
        }

        pub fn unmap(&mut self) {
            // SAFETY: unmaps exactly what `new` mapped, once.
            unsafe { libc::munmap(self.ptr.cast(), self.len) };
        }
    }
}

#[cfg(windows)]
mod os {
    use std::fs::File;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Memory::{
        CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS,
        MapViewOfFile, PAGE_READONLY, PAGE_READWRITE, UnmapViewOfFile,
    };

    pub struct Map {
        view: MEMORY_MAPPED_VIEW_ADDRESS,
        mapping: HANDLE,
    }

    // SAFETY: the mapping is shared memory owned by this value; every access
    // to it goes through `Raw`'s atomics and raw copies.
    unsafe impl Send for Map {}
    // SAFETY: as above.
    unsafe impl Sync for Map {}

    impl Map {
        pub fn new(file: &File, len: usize, writable: bool) -> std::io::Result<Map> {
            let (page, access) = if writable {
                (PAGE_READWRITE, FILE_MAP_READ | FILE_MAP_WRITE)
            } else {
                (PAGE_READONLY, FILE_MAP_READ)
            };
            // SAFETY: a valid file handle of at least `len` bytes.
            let mapping = unsafe {
                CreateFileMappingW(
                    file.as_raw_handle() as HANDLE,
                    core::ptr::null(),
                    page,
                    (len as u64 >> 32) as u32,
                    len as u32,
                    core::ptr::null(),
                )
            };
            if mapping.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: a valid mapping of at least `len` bytes.
            let view = unsafe { MapViewOfFile(mapping, access, 0, 0, len) };
            if view.Value.is_null() {
                let e = std::io::Error::last_os_error();
                // SAFETY: the handle from above.
                unsafe { CloseHandle(mapping) };
                return Err(e);
            }
            Ok(Map { view, mapping })
        }

        pub fn ptr(&self) -> *mut u8 {
            self.view.Value.cast()
        }

        pub fn unmap(&mut self) {
            // SAFETY: releases exactly what `new` created, once.
            unsafe {
                UnmapViewOfFile(self.view);
                CloseHandle(self.mapping);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file's path, removed when the test ends, whether it passes or fails.
    struct Temp(std::path::PathBuf);

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    impl std::ops::Deref for Temp {
        type Target = std::path::Path;
        fn deref(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl AsRef<std::path::Path> for Temp {
        fn as_ref(&self) -> &std::path::Path {
            &self.0
        }
    }

    fn temp(name: &str) -> Temp {
        let p = std::env::temp_dir().join(format!("nanoprobe-map-{}-{name}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        Temp(p)
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot map files")]
    fn two_mappings_share_bytes_and_the_file_never_shrinks() {
        let p = temp("share");
        let a = MappedFile::create(&p, 8192).expect("create");
        let b = MappedFile::open(&p).expect("open");
        a.store(4096, 42, Ordering::Release);
        assert_eq!(b.load(4096, Ordering::Acquire), 42);
        drop(a);
        let c = MappedFile::create(&p, 4096).expect("create smaller");
        assert_eq!(c.file_len(), 8192, "the file keeps its size");
        assert_eq!(c.len(), 4096, "the mapping is the part in use");
        let r = MappedFile::open_read_only(&p).expect("read only");
        assert_eq!(r.load(4096, Ordering::Acquire), 42);
        drop((b, c, r));
        std::fs::remove_file(&p).expect("remove");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot map files")]
    #[should_panic(expected = "read-only")]
    fn a_read_only_mapping_refuses_stores() {
        let p = temp("ro");
        drop(MappedFile::create(&p, 4096).expect("create"));
        let r = MappedFile::open_read_only(&p).expect("read only");
        let _ = std::fs::remove_file(&p);
        r.store(0, 1, Ordering::Relaxed);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot map files")]
    fn a_file_smaller_than_a_status_block_is_refused() {
        let p = temp("small");
        std::fs::write(&p, [0u8; 100]).expect("write");
        assert!(MappedFile::open(&p).is_err());
        std::fs::remove_file(&p).expect("remove");
    }
}
