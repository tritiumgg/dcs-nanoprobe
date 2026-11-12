//! The Windows side: the DLL inside DCS, on Windows and under Wine.

use core::ffi::c_void;
use core::fmt::Write as _;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use windows_sys::Win32::Foundation::{
    CloseHandle, FILETIME, HANDLE, HMODULE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_APPEND_DATA, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_ALWAYS, WriteFile,
};
use windows_sys::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, EXCEPTION_POINTERS, RtlCaptureStackBackTrace,
};
use windows_sys::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_PIN,
    GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW, GetModuleHandleExW,
};
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;

use crate::state;

/// The calling thread's ID.
pub fn thread_id() -> u64 {
    // SAFETY: no preconditions.
    u64::from(unsafe { GetCurrentThreadId() })
}

/// `QueryPerformanceCounter`.
pub fn ticks() -> i64 {
    let mut t = 0;
    // SAFETY: `t` is a valid out-pointer. The call cannot fail on XP or later.
    unsafe { QueryPerformanceCounter(&mut t) };
    t
}

/// `QueryPerformanceFrequency`, read once.
pub fn frequency() -> i64 {
    static FREQUENCY: OnceLock<i64> = OnceLock::new();
    *FREQUENCY.get_or_init(|| {
        let mut f = 0;
        // SAFETY: `f` is a valid out-pointer.
        unsafe { QueryPerformanceFrequency(&mut f) };
        f.max(1)
    })
}

/// An address inside this module.
fn here() -> *const u16 {
    static ANCHOR: u8 = 0;
    (&raw const ANCHOR).cast()
}

/// The module that contains `address`, without changing its reference count.
fn module_of(address: *const c_void) -> HMODULE {
    let mut module: HMODULE = core::ptr::null_mut();
    // SAFETY: FROM_ADDRESS takes any address; `module` is a valid out-pointer.
    let ok = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            address.cast(),
            &mut module,
        )
    };
    if ok == 0 {
        core::ptr::null_mut()
    } else {
        module
    }
}

/// This module's base address.
fn base() -> usize {
    module_of(here().cast()) as usize
}

/// Keep this module loaded until the process ends, whatever unloads it.
pub fn pin() -> bool {
    let mut module: HMODULE = core::ptr::null_mut();
    // SAFETY: as in `module_of`; PIN never fails on a loaded module.
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
            here(),
            &mut module,
        ) != 0
    }
}

/// Write the file name of `module`, without its folder, as ASCII into `out`.
/// Other characters become `?`.
fn file_name(module: HMODULE, out: &mut impl core::fmt::Write) {
    let mut wide = [0u16; 520];
    // SAFETY: `wide` is a valid buffer of the length given.
    let n = unsafe { GetModuleFileNameW(module, wide.as_mut_ptr(), wide.len() as u32) } as usize;
    let path = &wide[..n.min(wide.len())];
    let start = path
        .iter()
        .rposition(|&c| c == u16::from(b'\\') || c == u16::from(b'/'))
        .map_or(0, |i| i + 1);
    for &c in &path[start..] {
        let ch = if (0x20..0x7f).contains(&c) && c != u16::from(b'"') && c != u16::from(b'\\') {
            c as u8 as char
        } else {
            '?'
        };
        let _ = out.write_char(ch);
    }
}

/// This module's file name, without its folder.
pub fn module_name() -> String {
    let mut name = String::new();
    file_name(base() as HMODULE, &mut name);
    name
}

/// The CodeView signature of the PDB that matches this build, as symbol
/// servers write it: the GUID in 32 hex digits, then the age.
pub fn pdb_id() -> String {
    // SAFETY: reads this module's own mapped PE headers, which the loader
    // validated; every offset is bounds-checked against the image size.
    unsafe { read_pdb_id(base()) }.unwrap_or_default()
}

/// The CodeView signature from the PE headers of the module at `base`.
///
/// # Safety
///
/// `base` is zero or the base of a loaded PE32+ module.
unsafe fn read_pdb_id(base: usize) -> Option<String> {
    if base == 0 {
        return None;
    }
    let read_u32 = |offset: usize| {
        // SAFETY: the caller's module; every offset read is inside its
        // headers or checked against SizeOfImage below.
        unsafe { core::ptr::read_unaligned((base + offset) as *const u32) }
    };
    let read_u16 = |offset: usize| {
        // SAFETY: as for `read_u32`.
        unsafe { core::ptr::read_unaligned((base + offset) as *const u16) }
    };
    let nt = read_u32(0x3c) as usize;
    if read_u32(nt) != 0x0000_4550 {
        return None;
    }
    let optional = nt + 24;
    // PE32+ only.
    if read_u16(optional) != 0x20b {
        return None;
    }
    let image_size = read_u32(optional + 56) as usize;
    // The debug directory is data directory 6; the directories start at 112
    // in a PE32+ optional header.
    let directory = optional + 112 + 6 * 8;
    let (rva, size) = (
        read_u32(directory) as usize,
        read_u32(directory + 4) as usize,
    );
    if rva == 0 || rva + size > image_size {
        return None;
    }
    for entry in (rva..rva + size).step_by(28) {
        // IMAGE_DEBUG_TYPE_CODEVIEW is 2; AddressOfRawData is at 20.
        if read_u32(entry + 12) != 2 {
            continue;
        }
        let data = read_u32(entry + 20) as usize;
        if data == 0 || data + 24 > image_size || read_u32(data) != 0x5344_5352 {
            continue;
        }
        let guid = data + 4;
        let mut out = String::with_capacity(40);
        let _ = write!(out, "{:08X}", read_u32(guid));
        let _ = write!(out, "{:04X}", read_u16(guid + 4));
        let _ = write!(out, "{:04X}", read_u16(guid + 6));
        for i in 8..16 {
            // SAFETY: inside the CodeView record checked above.
            let b = unsafe { *((base + guid + i) as *const u8) };
            let _ = write!(out, "{b:02X}");
        }
        let _ = write!(out, "{:X}", read_u32(data + 20));
        return Some(out);
    }
    None
}

/// The crash file's path, as a NUL-terminated wide string.
static CRASH_PATH: OnceLock<Vec<u16>> = OnceLock::new();
/// The PDB signature, read before any crash.
static PDB: OnceLock<String> = OnceLock::new();
/// This module's base address and size.
static MODULE: OnceLock<(usize, usize)> = OnceLock::new();
/// How many records the handler has written. It stops at [`MAX_RECORDS`].
static RECORDS: AtomicU32 = AtomicU32::new(0);
const MAX_RECORDS: u32 = 8;
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Record native exceptions to `path`: those raised inside this module, and
/// those raised anywhere while an agent call is running. The handler records
/// and passes the exception on; it never handles one.
pub fn install_crash_handler(path: &Path) -> bool {
    let wide: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let _ = CRASH_PATH.set(wide);
    let _ = PDB.set(pdb_id());
    let b = base();
    // SAFETY: as in `read_pdb_id`.
    let size = if b == 0 {
        0
    } else {
        // SAFETY: SizeOfImage in this loaded module's PE32+ optional header.
        (unsafe { core::ptr::read_unaligned((b + read_nt(b) + 24 + 56) as *const u32) }) as usize
    };
    let _ = MODULE.set((b, size));
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return true;
    }
    // SAFETY: registers a handler that lives as long as the pinned module.
    // 0: called after the handlers already registered, so it sees only what
    // they pass on.
    !unsafe { AddVectoredExceptionHandler(0, Some(handler)) }.is_null()
}

fn read_nt(base: usize) -> usize {
    // SAFETY: the DOS header of this loaded module.
    unsafe { core::ptr::read_unaligned((base + 0x3c) as *const u32) as usize }
}

const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

/// The vectored handler. It allocates nothing and takes no lock.
unsafe extern "system" fn handler(info: *mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY: Windows passes valid pointers.
    let record = unsafe { &*(*info).ExceptionRecord };
    let code = record.ExceptionCode as u32;
    // Error severity only. C++ exceptions, Rust panics (0xE06D7363), longjmp
    // and debug messages are not.
    if code & 0xF000_0000 != 0xC000_0000 {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    let address = record.ExceptionAddress as usize;
    let (base, size) = MODULE.get().copied().unwrap_or((0, 0));
    let in_module = base != 0 && address >= base && address < base + size;
    let depth = state::depth();
    if !in_module && depth == 0 {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    if RECORDS.fetch_add(1, Ordering::Relaxed) >= MAX_RECORDS {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    let mut line = Buffer::new();
    write_record(&mut line, code, address, in_module, depth);
    if let Some(path) = CRASH_PATH.get() {
        append(path, line.bytes());
    }
    EXCEPTION_CONTINUE_SEARCH
}

fn write_record(out: &mut Buffer, code: u32, address: usize, in_module: bool, depth: u32) {
    let _ = out.write_str("{\"wall\":\"");
    wall(out);
    let _ = write!(
        out,
        "\",\"level\":\"error\",\"target\":\"agent\",\"msg\":\"native exception\",\"thread\":{}",
        thread_id()
    );
    let _ = write!(
        out,
        ",\"data\":{{\"code\":\"0x{code:08X}\",\"address\":\"0x{address:X}\",\"in_agent\":{in_module},\"depth\":{depth},\"module\":\""
    );
    location(out, address);
    let _ = write!(
        out,
        "\",\"pdb\":\"{}\",\"crumb_code\":{},\"crumb\":\"",
        PDB.get().map_or("", String::as_str),
        state::crumb_code()
    );
    state::crumb_text(|b| {
        for &c in b {
            let ch = if (0x20..0x7f).contains(&c) && c != b'"' && c != b'\\' {
                c as char
            } else {
                '?'
            };
            let _ = out.write_char(ch);
        }
    });
    let _ = out.write_str("\",\"frames\":\"");
    let mut frames = [core::ptr::null_mut::<c_void>(); 48];
    // SAFETY: `frames` is a valid buffer of the length given.
    let n = unsafe {
        RtlCaptureStackBackTrace(
            0,
            frames.len() as u32,
            frames.as_mut_ptr(),
            core::ptr::null_mut(),
        )
    } as usize;
    for (i, frame) in frames[..n].iter().enumerate() {
        if i > 0 {
            let _ = out.write_char(' ');
        }
        location(out, *frame as usize);
    }
    let _ = out.write_str("\"}}\n");
}

/// `module+0xoffset` for an address, or the bare address.
fn location(out: &mut Buffer, address: usize) {
    let module = module_of(address as *const c_void);
    if module.is_null() {
        let _ = write!(out, "0x{address:X}");
        return;
    }
    file_name(module, out);
    let _ = write!(out, "+0x{:X}", address - module as usize);
}

/// The UTC wall time, RFC 3339 with milliseconds, with no allocation.
fn wall(out: &mut Buffer) {
    let mut ft = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    // SAFETY: `ft` is a valid out-pointer.
    unsafe { GetSystemTimeAsFileTime(&mut ft) };
    let hundreds = (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime);
    // 100 ns units since 1601 to milliseconds since 1970.
    let ms = (hundreds / 10_000).saturating_sub(11_644_473_600_000);
    let (secs, millis) = (ms / 1000, ms % 1000);
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Civil from days, Howard Hinnant's algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let _ = write!(
        out,
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    );
}

/// Append `bytes` to the file at `path`, opened and closed here.
fn append(path: &[u16], bytes: &[u8]) {
    // SAFETY: `path` is NUL-terminated; the handle is closed below.
    unsafe {
        let file: HANDLE = CreateFileW(
            path.as_ptr(),
            FILE_APPEND_DATA,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            core::ptr::null(),
            OPEN_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            core::ptr::null_mut(),
        );
        if file == INVALID_HANDLE_VALUE {
            return;
        }
        let mut written = 0;
        WriteFile(
            file,
            bytes.as_ptr(),
            bytes.len() as u32,
            &mut written,
            core::ptr::null_mut(),
        );
        CloseHandle(file);
    }
}

/// A fixed buffer that drops what does not fit.
struct Buffer {
    bytes: [u8; 4096],
    len: usize,
}

impl Buffer {
    fn new() -> Buffer {
        Buffer {
            bytes: [0; 4096],
            len: 0,
        }
    }

    fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

impl core::fmt::Write for Buffer {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        // Keep room for the closing `"}}` and newline.
        let room = self.bytes.len().saturating_sub(self.len + 8);
        let n = s.len().min(room);
        self.bytes[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

/// A deliberate access violation, for the crash capture test.
pub fn fault() {
    // SAFETY: deliberately not safe; this is the fault the test needs.
    unsafe { core::ptr::write_volatile(core::ptr::null_mut::<u64>(), 1) }
}
