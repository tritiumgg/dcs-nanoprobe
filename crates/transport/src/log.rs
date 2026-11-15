//! The structured log record of `spec/log.md`, and a file sink that rotates at
//! its cap.
//!
//! The agent and the sidecar both write it. Encoding allocates one string per
//! record and takes no lock; the sink writes and flushes once per record.

use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// How severe a record is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// Something failed.
    Error,
    /// Something is wrong but work goes on.
    Warn,
    /// Normal operation.
    Info,
    /// Detail for development.
    Debug,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
        }
    }
}

/// A value in a record's `data` object.
#[derive(Clone, Debug, PartialEq)]
pub enum Value<'a> {
    /// A string.
    Str(&'a str),
    /// A number. Not finite is written as null.
    Num(f64),
    /// An integer.
    Int(i64),
    /// A boolean.
    Bool(bool),
    /// Null.
    Null,
}

/// One record. Optional keys are `None` when the writer has no value.
#[derive(Clone, Debug)]
pub struct Record<'a> {
    /// Milliseconds since the Unix epoch, UTC.
    pub wall_ms: u64,
    /// Severity.
    pub level: Level,
    /// The writer: `agent` or `sidecar` from Rust.
    pub target: &'a str,
    /// What happened.
    pub msg: &'a str,
    /// DCS real time, seconds.
    pub real: Option<f64>,
    /// Model time, seconds.
    pub model: Option<f64>,
    /// Mission generation.
    pub generation: Option<u64>,
    /// OS thread ID.
    pub thread: Option<u64>,
    /// Build hash.
    pub build: Option<&'a str>,
    /// Further values, written in this order.
    pub data: &'a [(&'a str, Value<'a>)],
}

impl<'a> Record<'a> {
    /// A record stamped now, with no optional keys.
    pub fn now(level: Level, target: &'a str, msg: &'a str) -> Self {
        Record {
            wall_ms: now_ms(),
            level,
            target,
            msg,
            real: None,
            model: None,
            generation: None,
            thread: None,
            build: None,
            data: &[],
        }
    }

    /// The record as one JSON line, ending in a newline.
    pub fn encode(&self) -> String {
        let mut out = String::with_capacity(128 + self.msg.len());
        out.push_str("{\"wall\":\"");
        rfc3339_ms(&mut out, self.wall_ms);
        out.push_str("\",\"level\":\"");
        out.push_str(self.level.as_str());
        out.push_str("\",\"target\":");
        string(&mut out, self.target);
        out.push_str(",\"msg\":");
        string(&mut out, self.msg);
        if let Some(real) = self.real {
            out.push_str(",\"real\":");
            number(&mut out, real);
        }
        if let Some(model) = self.model {
            out.push_str(",\"model\":");
            number(&mut out, model);
        }
        if let Some(generation) = self.generation {
            let _ = write!(out, ",\"gen\":{generation}");
        }
        if let Some(thread) = self.thread {
            let _ = write!(out, ",\"thread\":{thread}");
        }
        if let Some(build) = self.build {
            out.push_str(",\"build\":");
            string(&mut out, build);
        }
        if !self.data.is_empty() {
            out.push_str(",\"data\":{");
            for (i, (key, value)) in self.data.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                string(&mut out, key);
                out.push(':');
                match value {
                    Value::Str(s) => string(&mut out, s),
                    Value::Num(n) => number(&mut out, *n),
                    Value::Int(n) => {
                        let _ = write!(out, "{n}");
                    }
                    Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
                    Value::Null => out.push_str("null"),
                }
            }
            out.push('}');
        }
        out.push_str("}\n");
        out
    }
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn number(out: &mut String, n: f64) {
    if !n.is_finite() {
        out.push_str("null");
    } else if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
        let _ = write!(out, "{}", n as i64);
    } else {
        let _ = write!(out, "{n}");
    }
}

/// Write `ms` as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
fn rfc3339_ms(out: &mut String, ms: u64) {
    let secs = ms / 1000;
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    let _ = write!(
        out,
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60,
        ms % 1000
    );
}

/// The proleptic Gregorian date of a day count since 1970-01-01
/// (Howard Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The default cap of a log file, 4 MiB.
pub const DEFAULT_CAP: u64 = 4 * 1024 * 1024;

/// An append-only log file that rotates to `<path>.1` at its cap.
#[derive(Debug)]
pub struct FileSink {
    path: PathBuf,
    cap: u64,
    file: Option<File>,
    size: u64,
}

impl FileSink {
    /// Open `path` for appending.
    pub fn open(path: impl Into<PathBuf>, cap: u64) -> io::Result<Self> {
        let path = path.into();
        let file = append(&path)?;
        let size = file.metadata()?.len();
        Ok(FileSink {
            path,
            cap,
            file: Some(file),
            size,
        })
    }

    /// The file being written.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write one record and flush it. When the record would take the file
    /// past its cap, and the file is not empty, rotate first.
    pub fn write(&mut self, record: &Record<'_>) -> io::Result<()> {
        self.write_lines([record.encode().as_str()])
    }

    /// Write records already encoded by [`Record::encode`], each a JSON line
    /// with its newline, and flush once after the last. Each line rotates the
    /// file as [`FileSink::write`] does.
    pub fn write_lines<'l>(&mut self, lines: impl IntoIterator<Item = &'l str>) -> io::Result<()> {
        let mut wrote = false;
        for line in lines {
            if self.size > 0 && self.size + line.len() as u64 > self.cap {
                self.rotate()?;
            }
            let file = match &mut self.file {
                Some(file) => file,
                None => self.file.insert(append(&self.path)?),
            };
            file.write_all(line.as_bytes())?;
            self.size += line.len() as u64;
            wrote = true;
        }
        if wrote && let Some(file) = &mut self.file {
            file.flush()?;
        }
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        let mut old = self.path.clone().into_os_string();
        old.push(".1");
        // Windows refuses to rename an open file, or onto an existing one.
        self.file = None;
        let _ = std::fs::remove_file(&old);
        std::fs::rename(&self.path, &old)?;
        self.file = Some(append(&self.path)?);
        self.size = 0;
        Ok(())
    }
}

fn append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_every_key_in_order() {
        let record = Record {
            wall_ms: 1_794_268_800_123,
            level: Level::Warn,
            target: "agent",
            msg: "a \"quoted\"\nline\u{1}",
            real: Some(1.5),
            model: Some(f64::NAN),
            generation: Some(3),
            thread: Some(77),
            build: Some("abc"),
            data: &[
                ("s", Value::Str("x")),
                ("i", Value::Int(-2)),
                ("b", Value::Bool(true)),
                ("n", Value::Null),
                ("f", Value::Num(0.25)),
            ],
        };
        assert_eq!(
            record.encode(),
            "{\"wall\":\"2026-11-10T00:00:00.123Z\",\"level\":\"warn\",\"target\":\"agent\",\
             \"msg\":\"a \\\"quoted\\\"\\nline\\u0001\",\"real\":1.5,\"model\":null,\"gen\":3,\
             \"thread\":77,\"build\":\"abc\",\
             \"data\":{\"s\":\"x\",\"i\":-2,\"b\":true,\"n\":null,\"f\":0.25}}\n"
        );
    }

    #[test]
    fn leaves_out_absent_keys() {
        let mut record = Record::now(Level::Info, "sidecar", "up");
        record.wall_ms = 0;
        assert_eq!(
            record.encode(),
            "{\"wall\":\"1970-01-01T00:00:00.000Z\",\"level\":\"info\",\
             \"target\":\"sidecar\",\"msg\":\"up\"}\n"
        );
    }

    #[test]
    fn dates_cross_leap_years() {
        let mut out = String::new();
        // 2024-02-29T23:59:59.999Z
        rfc3339_ms(&mut out, 1_709_251_199_999);
        assert_eq!(out, "2024-02-29T23:59:59.999Z");
    }

    #[test]
    fn rotates_at_the_cap() {
        /// Removes the folder when the test ends, whether it passes or fails.
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch =
            Scratch(std::env::temp_dir().join(format!("nanoprobe-log-{}", std::process::id())));
        let dir = scratch.0.clone();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent.log");

        let mut sink = FileSink::open(&path, 300).unwrap();
        for i in 0..10 {
            let msg = format!("line {i}");
            sink.write(&Record::now(Level::Info, "agent", &msg))
                .unwrap();
        }
        drop(sink);

        let live = std::fs::read_to_string(&path).unwrap();
        let old = std::fs::read_to_string(dir.join("agent.log.1")).unwrap();
        assert!(live.len() <= 300 && old.len() <= 300);
        assert!(live.contains("line 9"));
    }
}
