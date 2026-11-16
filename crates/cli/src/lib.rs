//! The `nanoprobe` command's decoding and rendering.
//!
//! `inspect` prints a snapshot of the transport file: the status block, the
//! metrics and the unread records of both rings. `top` shows the live state and
//! its rates. Both open the file read-only and decode private copies of what
//! they read. `watch` prints a sidecar's world changes or events from its
//! API, one message per line. `doctor` checks an install from outside DCS, and
//! `install` and `uninstall` add the release to a Saved Games profile and take
//! it out again. `restore` restores a campaign's backup with the sidecar
//! stopped.

pub mod args;
pub mod dcslog;
pub mod decode;
pub mod doctor;
pub mod inspect;
pub mod profiles;
pub mod restore;
pub mod setup;
pub mod top;
pub mod util;
pub mod watch;

use std::io::Write;
use std::time::{Duration, Instant};

use args::Command;
use nanoprobe_transport::map::MappedFile;

/// Run the command with `args` (without the program name). Returns the exit
/// code: 0 on success, 2 for a usage error or, for `doctor`, nothing to check,
/// and 1 for a failure.
pub fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let command = match args::parse(args) {
        Ok(c) => c,
        Err(message) => {
            let _ = writeln!(err, "nanoprobe: {message}\n\n{}", args::USAGE);
            return 2;
        }
    };
    match command {
        Command::Help => {
            let _ = write!(out, "{}", args::USAGE);
            0
        }
        Command::Watch(o) => watch::run(&o, out, err),
        Command::Restore(o) => restore::run(&o, out, err),
        Command::Doctor(o) => doctor::run(&o.place, &profiles::Env::current(), out, err),
        Command::Install(o) => setup::run_install(&o, &profiles::Env::current(), out, err),
        Command::Uninstall(o) => setup::run_uninstall(&o, &profiles::Env::current(), out, err),
        Command::Inspect { path, records } => {
            let map = match open(&path, err) {
                Some(m) => m,
                None => return 1,
            };
            let text = inspect::render(&map, map.file_len(), &inspect::Options { records });
            let _ = write!(out, "{text}");
            0
        }
        Command::Top {
            path,
            interval_ms,
            once,
        } => {
            let mut map = match open(&path, err) {
                Some(m) => m,
                None => return 1,
            };
            let mut top = top::Top::new(&path.display().to_string());
            let start = Instant::now();
            loop {
                // A new DCS process may have grown the file (spec/transport.md,
                // "The file"): map it again.
                if std::fs::metadata(&path).is_ok_and(|md| md.len() != map.file_len())
                    && let Ok(m) = MappedFile::open_read_only(&path)
                {
                    map = m;
                }
                let screen = top.render(&map, map.file_len(), start.elapsed());
                let written = if once {
                    write!(out, "{screen}")
                } else {
                    write!(out, "\x1b[2J\x1b[H{screen}")
                };
                if written.is_err() || out.flush().is_err() || once {
                    return 0;
                }
                std::thread::sleep(Duration::from_millis(interval_ms));
            }
        }
    }
}

fn open(path: &std::path::Path, err: &mut dyn Write) -> Option<MappedFile> {
    match MappedFile::open_read_only(path) {
        Ok(m) => Some(m),
        Err(e) => {
            let _ = writeln!(err, "nanoprobe: cannot open {}: {e}", path.display());
            None
        }
    }
}

#[cfg(test)]
mod tests;
