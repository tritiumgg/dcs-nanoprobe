//! The agent's process-wide state, shared by every Lua state that opens it.
//!
//! DCS runs every Lua state on one thread, so nothing here is contended. The
//! locks are there because Rust requires them for statics, and every one is
//! taken with `try_lock` where a crash or panic path could find it held.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, Once, OnceLock};

use nanoprobe_transport::log::{FileSink, Level, Record, Value};

use crate::platform;

/// The log's size cap, `spec/log.md`.
const LOG_CAP: u64 = 4 * 1024 * 1024;

/// Set when an entry point caught a panic. From then on every entry point
/// but `status` does nothing until DCS restarts.
static DISABLED: AtomicBool = AtomicBool::new(false);
/// Why the agent was disabled.
static REASON: Mutex<String> = Mutex::new(String::new());
/// How many agent calls are running. The crash handler records an exception
/// raised anywhere while this is above zero.
static DEPTH: AtomicU32 = AtomicU32::new(0);
/// The breadcrumb: a number and a short text, set before a risky call.
static CRUMB_CODE: AtomicU64 = AtomicU64::new(0);
static CRUMB_TEXT: [AtomicU8; CRUMB_MAX] = [const { AtomicU8::new(0) }; CRUMB_MAX];
static CRUMB_LEN: AtomicUsize = AtomicUsize::new(0);
const CRUMB_MAX: usize = 64;

/// The install root, `<profile>/nanoprobe/`, once configured.
static ROOT: Mutex<Option<PathBuf>> = Mutex::new(None);
/// The agent's log, once configured.
static LOG: Mutex<Option<FileSink>> = Mutex::new(None);
/// Whether the module is pinned.
static PINNED: AtomicBool = AtomicBool::new(false);
/// The build hash, from the DLL's file name.
static BUILD: OnceLock<String> = OnceLock::new();

/// Work done once per process, on the first open in any state.
pub fn once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        PINNED.store(platform::pin(), Ordering::Relaxed);
        std::panic::set_hook(Box::new(|info| {
            let location = info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()))
                .unwrap_or_default();
            let message = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_default();
            if let Ok(mut reason) = REASON.try_lock() {
                *reason = format!("panic at {location}: {message}");
            }
            log(
                Level::Error,
                "panic",
                &[
                    ("message", Value::Str(&message)),
                    ("location", Value::Str(&location)),
                    ("crumb_code", Value::Int(crumb_code() as i64)),
                ],
            );
        }));
    });
}

/// Whether the module is pinned.
pub fn pinned() -> bool {
    PINNED.load(Ordering::Relaxed)
}

/// The build hash: the part of the DLL's file name after `nanoprobe_agent_`,
/// or the file name when it does not have that form.
pub fn build() -> &'static str {
    BUILD.get_or_init(|| {
        let name = platform::module_name();
        let stem = name.rsplit_once('.').map_or(name.as_str(), |(s, _)| s);
        stem.strip_prefix("nanoprobe_agent_")
            .unwrap_or(stem)
            .to_owned()
    })
}

/// Point the agent at its install root: open `logs/agent.log` and record
/// native exceptions to `logs/crash.log`. A second call with the same root
/// changes nothing.
pub fn configure(root: &Path) -> Result<(), String> {
    let mut current = ROOT.lock().map_err(|_| "the root lock is poisoned")?;
    if current.as_deref() == Some(root) {
        return Ok(());
    }
    let logs = root.join("logs");
    std::fs::create_dir_all(&logs).map_err(|e| format!("{}: {e}", logs.display()))?;
    let sink = FileSink::open(logs.join("agent.log"), LOG_CAP)
        .map_err(|e| format!("{}: {e}", logs.join("agent.log").display()))?;
    *LOG.lock().map_err(|_| "the log lock is poisoned")? = Some(sink);
    platform::install_crash_handler(&logs.join("crash.log"));
    *current = Some(root.to_path_buf());
    Ok(())
}

/// The install root, if configured.
pub fn root() -> Option<PathBuf> {
    ROOT.try_lock().ok().and_then(|r| r.clone())
}

/// Write one record to the agent's log, if it is open. A failed write is
/// dropped: the log must never take the agent down.
pub fn log(level: Level, msg: &str, data: &[(&str, Value<'_>)]) {
    let Ok(mut guard) = LOG.try_lock() else {
        return;
    };
    let Some(sink) = guard.as_mut() else {
        return;
    };
    let mut record = Record::now(level, "agent", msg);
    record.thread = Some(platform::thread_id());
    record.build = Some(build());
    record.data = data;
    let _ = sink.write(&record);
}

/// Whether a panic disabled the agent.
pub fn disabled() -> bool {
    DISABLED.load(Ordering::Relaxed)
}

/// Why the agent was disabled, or empty.
pub fn reason() -> String {
    REASON.try_lock().map(|r| r.clone()).unwrap_or_default()
}

/// Disable the agent after a caught panic.
pub fn trip(entry: &str) {
    DISABLED.store(true, Ordering::Relaxed);
    log(
        Level::Error,
        "disabled after a panic",
        &[("entry", Value::Str(entry))],
    );
}

/// Mark an agent call as running.
pub fn enter() {
    DEPTH.fetch_add(1, Ordering::Relaxed);
}

/// Mark an agent call as finished.
pub fn leave() {
    DEPTH.fetch_sub(1, Ordering::Relaxed);
}

/// How many agent calls are running.
#[cfg_attr(unix, allow(dead_code, reason = "the crash handler is Windows-only"))]
pub fn depth() -> u32 {
    DEPTH.load(Ordering::Relaxed)
}

/// Set the breadcrumb's number. Cheap enough for every row of a sweep.
pub fn crumb(code: u64) {
    CRUMB_CODE.store(code, Ordering::Relaxed);
}

/// Set the breadcrumb's text, cut to 64 bytes.
pub fn crumb_text_set(text: &[u8]) {
    let n = text.len().min(CRUMB_MAX);
    for (slot, &b) in CRUMB_TEXT.iter().zip(&text[..n]) {
        slot.store(b, Ordering::Relaxed);
    }
    CRUMB_LEN.store(n, Ordering::Relaxed);
}

/// The breadcrumb's number.
pub fn crumb_code() -> u64 {
    CRUMB_CODE.load(Ordering::Relaxed)
}

/// Call `f` with the breadcrumb's text, without allocating.
#[cfg_attr(unix, allow(dead_code, reason = "the crash handler is Windows-only"))]
pub fn crumb_text(f: impl FnOnce(&[u8])) {
    let mut bytes = [0u8; CRUMB_MAX];
    let n = CRUMB_LEN.load(Ordering::Relaxed).min(CRUMB_MAX);
    for (out, slot) in bytes.iter_mut().zip(&CRUMB_TEXT[..n]) {
        *out = slot.load(Ordering::Relaxed);
    }
    f(&bytes[..n]);
}
