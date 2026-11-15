//! The platform's lines in DCS's `dcs.log`.
//!
//! The loader and the hook write with `log.write('NANOPROBE', level, text)`,
//! which DCS prints as `<date> <time> <LEVEL> NANOPROBE (<thread>): <text>`
//! (LOG). A DCS start begins with a `=== Log opened` line (LOG). Only lines
//! whose subsystem is exactly `NANOPROBE` are returned; development
//! subsystems such as `NANOPROBE-DEV` and every other line are skipped.

/// One platform line.
#[derive(Debug, PartialEq, Eq)]
pub struct Line {
    /// DCS's level word: `INFO`, `WARNING`, `ERROR`, and so on.
    pub level: String,
    /// The text after the subsystem.
    pub message: String,
}

/// What starts a DCS start in the log.
const OPENED: &str = "=== Log opened";
/// The subsystem.
const SUBSYSTEM: &str = "NANOPROBE";

/// The platform's lines after the last `=== Log opened` line, or in the
/// whole log when it has none.
pub fn newest(log: &[u8]) -> Vec<Line> {
    let text = String::from_utf8_lossy(log);
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.iter().rposition(|l| l.contains(OPENED)).unwrap_or(0);
    lines[start..].iter().filter_map(|l| parse(l)).collect()
}

/// A line written under the `NANOPROBE` subsystem.
pub fn parse(line: &str) -> Option<Line> {
    let line = line.trim_end_matches('\r');
    let words: Vec<(usize, &str)> = line
        .split(' ')
        .scan(0, |at, word| {
            let start = *at;
            *at += word.len() + 1;
            Some((start, word))
        })
        .filter(|(_, w)| !w.is_empty())
        .collect();
    // Date, time, level, then the subsystem.
    let index = 3;
    let name = words.get(index)?.1.split(['(', ':']).next().unwrap_or("");
    if name != SUBSYSTEM {
        return None;
    }
    let level = words[index - 1].1.to_owned();
    let after = &line[words[index].0 + SUBSYSTEM.len()..];
    let message = match after.find("): ") {
        Some(at) if after.trim_start().starts_with('(') => &after[at + 3..],
        _ => after.trim_start().strip_prefix(':').unwrap_or(after),
    };
    Some(Line {
        level,
        message: message.trim().to_owned(),
    })
}

/// The session ID and the build in the hook's start line,
/// `session <id> build <build>`.
pub fn session(message: &str) -> Option<(&str, &str)> {
    let rest = message.strip_prefix("session ")?;
    let (id, build) = rest.split_once(" build ")?;
    let build = build.trim();
    let word = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric());
    (word(id) && word(build)).then_some((id, build))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_platform_line() {
        assert_eq!(
            parse("2026-11-11 10:00:00.123 INFO    NANOPROBE (Main): session 5f build 0123abcd"),
            Some(Line {
                level: "INFO".into(),
                message: "session 5f build 0123abcd".into()
            })
        );
        assert_eq!(
            parse("2026-11-11 10:00:00.123 ERROR   NANOPROBE: loader: no manifest at x"),
            Some(Line {
                level: "ERROR".into(),
                message: "loader: no manifest at x".into()
            })
        );
    }

    #[test]
    fn skips_other_subsystems() {
        assert_eq!(
            parse("2026-11-11 10:00:00.123 INFO    NANOPROBE-DEV (Main): loaded"),
            None
        );
        assert_eq!(
            parse("2026-11-11 10:00:00.123 INFO    EDCORE (Main): NANOPROBE text"),
            None
        );
        assert_eq!(parse("NANOPROBE"), None);
    }

    #[test]
    fn takes_the_newest_start() {
        let log = "\
=== Log opened UTC 2026-11-10 10:00:00
2026-11-10 10:00:01.000 ERROR   NANOPROBE (Main): loader: old failure
=== Log opened UTC 2026-11-11 10:00:00
2026-11-11 10:00:01.000 INFO    NANOPROBE (Main): session 1 build abc
";
        let lines = newest(log.as_bytes());
        assert_eq!(lines.len(), 1);
        assert_eq!(session(&lines[0].message), Some(("1", "abc")));
    }
}
