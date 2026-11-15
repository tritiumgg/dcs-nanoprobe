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

/// When the newest start began: the UTC time on the last
/// `=== Log opened UTC <YYYY-MM-DD HH:MM:SS>` line (LOG).
pub fn newest_opened(log: &[u8]) -> Option<std::time::SystemTime> {
    let text = String::from_utf8_lossy(log);
    let line = text.lines().rev().find(|l| l.contains(OPENED))?;
    let rest = line.split_once("UTC ")?.1.trim();
    let (date, time) = rest.split_once(' ')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.split(':').map(|p| p.parse::<i64>().ok());
    let (h, min, sec) = (t.next()??, t.next()??, t.next()??);
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) || h > 23 || min > 59 || sec > 60 {
        return None;
    }
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = u64::try_from(days * 86_400 + h * 3_600 + min * 60 + sec).ok()?;
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
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
    fn reads_when_the_newest_start_began() {
        let log =
            "=== Log opened UTC 2026-11-10 10:00:00\nx\n=== Log opened UTC 2026-11-15 12:30:05\n";
        let at = newest_opened(log.as_bytes()).unwrap();
        let secs = at.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(secs, 1_794_745_805);
        assert_eq!(newest_opened(b"no start\n"), None);
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
