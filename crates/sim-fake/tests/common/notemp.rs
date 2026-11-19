//! SQLite with no temporary folder, for a test process: [`install`] makes a
//! wrapper over the default VFS the default, and the wrapper refuses every
//! temporary file (a statement journal, a temporary database or journal, a
//! transient database) with `SQLITE_IOERR_GETTEMPPATH`, as SQLite does on a
//! host where none of its temporary folders is writable (a `FROM scratch`
//! image run as a user). Temporary data SQLite keeps in memory never reaches
//! the VFS, so a refusal is exactly what such a host would fail.

use std::ffi::{CStr, c_char, c_int};
use std::sync::{Mutex, Once};

use rusqlite::ffi;

/// The kinds of temporary file refused since the process started.
static REFUSED: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

const TEMP: c_int = ffi::SQLITE_OPEN_TEMP_DB
    | ffi::SQLITE_OPEN_TRANSIENT_DB
    | ffi::SQLITE_OPEN_TEMP_JOURNAL
    | ffi::SQLITE_OPEN_SUBJOURNAL;

fn kind(flags: c_int) -> &'static str {
    if flags & ffi::SQLITE_OPEN_SUBJOURNAL != 0 {
        "statement journal"
    } else if flags & ffi::SQLITE_OPEN_TEMP_JOURNAL != 0 {
        "temporary journal"
    } else if flags & ffi::SQLITE_OPEN_TRANSIENT_DB != 0 {
        "transient database"
    } else if flags & ffi::SQLITE_OPEN_TEMP_DB != 0 {
        "temporary database"
    } else {
        "unnamed file"
    }
}

unsafe extern "C" fn open(
    vfs: *mut ffi::sqlite3_vfs,
    name: ffi::sqlite3_filename,
    file: *mut ffi::sqlite3_file,
    flags: c_int,
    out: *mut c_int,
) -> c_int {
    if flags & TEMP != 0 || name.is_null() {
        REFUSED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(kind(flags));
        // SAFETY: `file` points to `szOsFile` writable bytes; a failed open
        // leaves `pMethods` null, so SQLite calls no method on it.
        unsafe { (*file).pMethods = std::ptr::null() };
        return ffi::SQLITE_IOERR_GETTEMPPATH;
    }
    // SAFETY: `pAppData` is the default VFS, which SQLite keeps for the
    // process; the wrapper has its `szOsFile`, so `file` fits its file.
    unsafe {
        let base = (*vfs).pAppData.cast::<ffi::sqlite3_vfs>();
        match (*base).xOpen {
            Some(f) => f(base, name, file, flags, out),
            None => ffi::SQLITE_ERROR,
        }
    }
}

/// Make the refusing VFS the process's default, once. Call it before the
/// test opens any connection.
pub fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: called once, before this process opens a connection;
        // the wrapper is leaked, so it outlives every connection, and
        // copies the default VFS's methods except `xOpen`.
        unsafe {
            assert_eq!(ffi::sqlite3_initialize(), ffi::SQLITE_OK);
            let base = ffi::sqlite3_vfs_find(std::ptr::null());
            assert!(!base.is_null(), "a default VFS");
            let name: &'static CStr = c"notemp";
            let mut v: ffi::sqlite3_vfs = *base;
            v.zName = name.as_ptr().cast::<c_char>();
            v.pAppData = base.cast();
            v.pNext = std::ptr::null_mut();
            v.xOpen = Some(open);
            let v = Box::leak(Box::new(v));
            assert_eq!(ffi::sqlite3_vfs_register(v, 1), ffi::SQLITE_OK);
        }
    });
}

/// The temporary files refused since the last call, and forget them.
pub fn take_refused() -> Vec<&'static str> {
    std::mem::take(&mut *REFUSED.lock().unwrap_or_else(|e| e.into_inner()))
}

/// That the refusing VFS is the one connections use and that it refuses: a
/// temporary table too large for its one-page cache must fail with
/// `SQLITE_IOERR_GETTEMPPATH`, and be counted. Call it after [`install`],
/// with a scratch folder; it forgets what it refused.
pub fn assert_refusing(dir: &std::path::Path) {
    let conn = rusqlite::Connection::open(dir.join("control.sqlite")).expect("open");
    let r = conn.execute_batch(
        "PRAGMA temp.cache_size = 1;
         CREATE TEMP TABLE t AS
           WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 20000)
           SELECT i, zeroblob(200) AS b FROM n;",
    );
    let code = match &r {
        Err(rusqlite::Error::SqliteFailure(e, _)) => e.extended_code,
        other => panic!("the control was not refused: {other:?}"),
    };
    assert_eq!(code, ffi::SQLITE_IOERR_GETTEMPPATH);
    assert!(!take_refused().is_empty(), "the refusal is counted");
}
