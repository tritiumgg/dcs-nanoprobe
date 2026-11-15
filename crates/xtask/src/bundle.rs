//! Session bundles: one zip per session, built from an allow-list and
//! redacted.
//!
//! What goes in, and nothing else:
//!
//! - every file in `<profile>/nanoprobe/logs/`;
//! - the part of `<profile>/Logs/dcs.log` from the last session marker the
//!   hook writes, which the target cuts out before sending, so earlier lines
//!   never leave it;
//! - `BUNDLE.txt`: when it was made, the target's DCS and Wine versions, the
//!   files included, and the redaction counts.
//!
//! Every line is redacted before it is written: IP addresses, player UCIDs
//! and player names become stable keyed hashes such as `ip-1a2b3c4d`. The key
//! is per machine and stays outside the repository, so a hash cannot be
//! reversed by hashing every IPv4 address.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::{Cursor, Write as _};
use std::path::PathBuf;
use std::sync::LazyLock;

use hmac::{Hmac, Mac};
use regex::Regex;
use sha2::Sha256;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::install::Profile;

/// The two strings that, together on one `dcs.log` line, make the session
/// marker the hook writes with `log.write('NANOPROBE', ...)`.
pub const MARKER: [&str; 2] = ["NANOPROBE", ": session "];

const LOGS: &str = "nanoprobe/logs";
const DCS_LOG: &str = "Logs/dcs.log";

/// A player's name in `dcs.log` (LOG, DCS 2.9.31.29434 dedicated server,
/// Wine 10.0): `ASYNCNET (Main): added client[N] name=<name> addr=<ip:port>`.
/// Our own logs name a player under the `player` key.
static NAME_SOURCES: LazyLock<[Regex; 2]> = LazyLock::new(|| {
    [
        Regex::new(r"added client\[\d+\] name=(.+?) addr=").unwrap(),
        Regex::new(r#""player":"((?:[^"\\]|\\.)+)""#).unwrap(),
    ]
});

static IPV4: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:(?:25[0-5]|2[0-4]\d|1?\d?\d)\.){3}(?:25[0-5]|2[0-4]\d|1?\d?\d)\b").unwrap()
});

/// A run that may be an IPv6 address. [`ipv6`] checks it.
static IPV6_RUN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[0-9A-Fa-f:]{2,39}").unwrap());

/// Replace every IPv6 address in `text`. A candidate must parse as one, hold
/// a hex digit, and not touch a letter, digit or `_` on either side, so
/// `12:00:00` and `Ed::Logger` stay as they are.
fn ipv6(text: &str, mut replace: impl FnMut(&str) -> String) -> String {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == ':');
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for m in IPV6_RUN.find_iter(text) {
        let candidate = m.as_str();
        let isolated =
            !word(text[..m.start()].chars().next_back()) && !word(text[m.end()..].chars().next());
        if isolated
            && candidate.bytes().any(|b| b.is_ascii_hexdigit())
            && candidate.parse::<std::net::Ipv6Addr>().is_ok()
        {
            out.push_str(&text[last..m.start()]);
            out.push_str(&replace(candidate));
            last = m.end();
        }
    }
    out.push_str(&text[last..]);
    out
}

/// A DCS player UCID, or a player key (spec/world.md, "Players"): 32 hex
/// digits, in either case.
static UCID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[0-9a-fA-F]{32}\b").unwrap());

/// Replaces sensitive values with keyed hashes.
pub struct Redactor {
    key: Vec<u8>,
    names: Vec<String>,
    /// Secrets, such as a server's join password, replaced by `<secret>`
    /// before anything else.
    secrets: Vec<String>,
    /// How many values of each kind were replaced.
    pub counts: [usize; 4],
}

impl Redactor {
    /// A redactor with `key`, that also replaces every name `texts` reveal.
    pub fn new(key: &[u8], texts: &[&str]) -> Redactor {
        let mut names = BTreeSet::new();
        for text in texts {
            for source in NAME_SOURCES.iter() {
                for capture in source.captures_iter(text) {
                    let name = capture[1].trim();
                    if name.chars().count() >= 2 {
                        names.insert(name.to_owned());
                    }
                }
            }
        }
        // Longest first, so a name that contains another is replaced whole.
        let mut names: Vec<String> = names.into_iter().collect();
        names.sort_by_key(|name| std::cmp::Reverse(name.len()));
        Redactor {
            key: key.to_vec(),
            names,
            secrets: Vec::new(),
            counts: [0; 4],
        }
    }

    /// The same redactor, which also replaces each of `secrets` whole.
    pub fn with_secrets(mut self, secrets: Vec<String>) -> Redactor {
        self.secrets = secrets.into_iter().filter(|s| !s.is_empty()).collect();
        self.secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        self
    }

    fn token(&self, kind: &str, value: &str) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).expect("any key length");
        mac.update(kind.as_bytes());
        mac.update(&[0]);
        mac.update(value.as_bytes());
        let digest = mac.finalize().into_bytes();
        let mut out = format!("{kind}-");
        for b in &digest[..4] {
            let _ = write!(out, "{b:02x}");
        }
        out
    }

    /// `text` with every name, UCID and IP address replaced.
    pub fn redact(&mut self, text: &str) -> String {
        let mut out = text.to_owned();
        for secret in &self.secrets {
            out = out.replace(secret.as_str(), "<secret>");
        }
        for name in self.names.clone() {
            let n = out.matches(name.as_str()).count();
            if n > 0 {
                self.counts[0] += n;
                out = out.replace(name.as_str(), &self.token("player", &name));
            }
        }
        for (i, (kind, pattern)) in [("ucid", &*UCID), ("ip", &*IPV4)].into_iter().enumerate() {
            let mut n = 0;
            out = pattern
                .replace_all(&out, |c: &regex::Captures<'_>| {
                    n += 1;
                    self.token(kind, &c[0])
                })
                .into_owned();
            self.counts[i + 1] += n;
        }
        let mut n = 0;
        out = ipv6(&out, |address| {
            n += 1;
            self.token("ip", address)
        });
        self.counts[3] += n;
        out
    }
}

/// The per-machine redaction key, created on first use.
pub fn key() -> Result<Vec<u8>, String> {
    let path = crate::profile::config_dir()?.join("redact.key");
    if let Ok(key) = std::fs::read(&path)
        && key.len() >= 32
    {
        return Ok(key);
    }
    let mut key = vec![0u8; 32];
    getrandom::fill(&mut key).map_err(|e| format!("no random bytes for the key: {e}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(&path, &key).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(key)
}

/// The facts `BUNDLE.txt` records about the target.
#[derive(Debug, Default)]
pub struct Facts {
    /// The target kind, such as `docker-wine`.
    pub kind: String,
    /// `wine --version`, when the target runs Wine.
    pub wine: Option<String>,
    /// The DCS build and branch.
    pub dcs: Option<String>,
}

/// Collect, redact and zip a bundle from `profile`.
pub fn build(
    profile: &dyn Profile,
    facts: &Facts,
    key: &[u8],
    secrets: Vec<String>,
) -> Result<Vec<u8>, String> {
    let mut files: Vec<(String, String)> = Vec::new();
    let mut notes = Vec::new();

    for name in profile.list(LOGS)? {
        if name.contains('/') || name.contains('\\') {
            continue;
        }
        if let Some(bytes) = profile.read(&format!("{LOGS}/{name}"))? {
            files.push((
                format!("logs/{name}"),
                String::from_utf8_lossy(&bytes).into(),
            ));
        }
    }
    match profile.read_from_last(DCS_LOG, MARKER)? {
        Some(bytes) => files.push(("dcs.log".into(), String::from_utf8_lossy(&bytes).into())),
        None => notes.push("dcs.log: no session marker found, so none of it is included"),
    }

    let texts: Vec<&str> = files.iter().map(|(_, text)| text.as_str()).collect();
    let mut redactor = Redactor::new(key, &texts).with_secrets(secrets);
    let files: Vec<(String, String)> = files
        .iter()
        .map(|(name, text)| (name.clone(), redactor.redact(text)))
        .collect();

    let mut index = String::new();
    let _ = writeln!(index, "nanoprobe session bundle");
    let _ = writeln!(index, "created   {}", utc_now());
    let _ = writeln!(index, "target    {}", facts.kind);
    let _ = writeln!(
        index,
        "dcs       {}",
        facts.dcs.as_deref().unwrap_or("unknown")
    );
    let _ = writeln!(
        index,
        "wine      {}",
        facts.wine.as_deref().unwrap_or("none")
    );
    let [names, ucids, v4, v6] = redactor.counts;
    let _ = writeln!(
        index,
        "redacted  {names} player names, {ucids} UCIDs, {} IP addresses",
        v4 + v6
    );
    let _ = writeln!(index, "files");
    for (name, text) in &files {
        let _ = writeln!(index, "  {name} ({} bytes)", text.len());
    }
    for note in notes {
        let _ = writeln!(index, "note      {note}");
    }

    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, text) in [("BUNDLE.txt".to_owned(), index)].iter().chain(&files) {
        writer
            .start_file(name.as_str(), options)
            .map_err(|e| e.to_string())?;
        writer
            .write_all(text.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    Ok(writer.finish().map_err(|e| e.to_string())?.into_inner())
}

/// Where bundles go: `bundles/` in the repository, which git ignores.
pub fn folder() -> PathBuf {
    crate::repository().join("bundles")
}

/// UTC now as `YYYYMMDDTHHMMSSZ`.
pub fn stamp() -> String {
    utc_now().replace(['-', ':'], "")
}

/// UTC now as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = (secs / 86_400) as i64;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let rem = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use super::*;
    use crate::install::Local;

    const KEY: &[u8] = b"a test key of thirty-two bytes!!";

    /// Fake secrets, each of which must not appear anywhere in a bundle.
    const SECRETS: [&str; 9] = [
        "np-join-PLANTED-pw",
        "203.0.113.77",
        "198.51.100.4",
        "2001:db8::17",
        "0123456789abcdef0123456789abcdef",
        "Maverick Two",
        "Goose",
        "hunter2-server-password",
        "BEFORE-THE-MARKER",
    ];

    fn profile(dir: &std::path::Path) -> Local {
        let write = |path: &str, text: &str| {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(
            "Logs/dcs.log",
            "2026-11-10 10:00:00.000 INFO    APP (Main): BEFORE-THE-MARKER 198.51.100.4\n\
             2026-11-10 10:00:01.000 INFO    NANOPROBE (Main): session 0a1b build x\n\
             2026-11-10 10:00:02.000 INFO    ASYNCNET (Main): added client[2] name=Maverick Two addr=203.0.113.77:10308\n\
             2026-11-10 10:00:03.000 INFO    ASYNCNET (Main): added client[3] name=Goose addr=[2001:db8::17]:10308\n\
             2026-11-10 10:00:04.000 INFO    CHAT (Main): Goose: hello Maverick Two\n\
             2026-11-10 10:00:05.000 INFO    APP (Main): 12:00:00 is a time in Ed::Logger\n\
             2026-11-10 10:00:06.000 INFO    NET (Main): joining with np-join-PLANTED-pw\n",
        );
        write(
            "nanoprobe/logs/hook.log",
            "{\"msg\":\"slot\",\"data\":{\"player\":\"Goose\",\"ucid\":\"0123456789abcdef0123456789abcdef\"}}\n",
        );
        write(
            "Config/serverSettings.lua",
            "password = \"hunter2-server-password\"\n",
        );
        write("Config/network.vault", "hunter2-server-password");
        Local::new(dir).unwrap()
    }

    fn unzip(bytes: &[u8]) -> Vec<(String, String)> {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        (0..archive.len())
            .map(|i| {
                let mut file = archive.by_index(i).unwrap();
                let mut text = String::new();
                file.read_to_string(&mut text).unwrap();
                (file.name().unwrap().into_owned(), text)
            })
            .collect()
    }

    /// A scratch folder, removed when the test ends, whether it passes or fails.
    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn no_planted_secret_survives() {
        let scratch =
            Scratch(std::env::temp_dir().join(format!("nanoprobe-bundle-{}", std::process::id())));
        let dir = scratch.0.clone();
        let _ = std::fs::remove_dir_all(&dir);
        let local = profile(&dir);

        // A join password, as `client join` would pass it (client.rs).
        let bytes = build(
            &local,
            &Facts::default(),
            KEY,
            vec!["np-join-PLANTED-pw".into()],
        )
        .unwrap();
        let files = unzip(&bytes);

        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["BUNDLE.txt", "logs/hook.log", "dcs.log"]);
        for (name, text) in &files {
            for secret in SECRETS {
                assert!(
                    !text.contains(secret),
                    "{secret:?} survived in {name}:\n{text}"
                );
            }
        }

        let dcs = &files[2].1;
        assert!(
            dcs.starts_with("2026-11-10 10:00:01.000 INFO    NANOPROBE"),
            "{dcs}"
        );
        assert!(dcs.contains("12:00:00 is a time in Ed::Logger"), "{dcs}");
        let goose = Redactor::new(KEY, &[]).token("player", "Goose");
        assert!(dcs.contains(&format!("{goose}: hello")), "{dcs}");
        assert!(
            files[1].1.contains(&goose),
            "the same name hashes the same everywhere"
        );
        assert!(
            files[0]
                .1
                .contains("redacted  5 player names, 1 UCIDs, 2 IP addresses"),
            "{}",
            files[0].1
        );
    }

    #[test]
    fn any_32_hex_digit_token_is_redacted_and_the_keyring_file_is_never_collected() {
        let dir = std::env::temp_dir().join(format!("nanoprobe-keyring-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ucid = "fedcba9876543210FEDCBA9876543210";
        let lower = "00112233445566778899aabbccddeeff";
        let write = |path: &str, text: &str| {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(
            "nanoprobe/logs/sidecar.log",
            &format!("pairs {{\"{lower}\":\"{ucid}\"}} but not {lower}0 or 0{lower}\n"),
        );
        // The server store and a `players.json` not yet migrated sit in the
        // store directory, which no collected path names; wherever they are
        // placed in the profile they stay out.
        let secret = "KEYRING-SECRET-PLANTED";
        for place in [
            "players.json",
            "nanoprobe/players.json",
            "campaigns/players.json",
            "server/store.sqlite",
            "nanoprobe/server/store.sqlite",
            "players.json.migrated",
            "server/events.sqlite",
            "server/backups/store-1.sqlite",
        ] {
            write(
                place,
                &format!("{{\"secret\":\"{secret}\",\"players\":{{\"{lower}\":\"{ucid}\"}}}}"),
            );
        }
        write(
            "Logs/dcs.log",
            "2026 INFO NANOPROBE (Main): session 1 build x\n",
        );
        let bytes = build(
            &Local::new(&dir).unwrap(),
            &Facts::default(),
            KEY,
            Vec::new(),
        )
        .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let files = unzip(&bytes);
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["BUNDLE.txt", "logs/sidecar.log", "dcs.log"]);
        for (name, text) in &files {
            assert!(!text.contains(secret), "{name}");
            assert!(!text.contains(ucid), "{name}: {text}");
        }
        let log = &files[1].1;
        assert_eq!(log.matches("ucid-").count(), 2, "{log}");
        // A longer or shorter run of hex digits is no ucid.
        assert!(
            log.contains(&format!("{lower}0")) && log.contains(&format!("0{lower}")),
            "{log}"
        );
    }

    #[test]
    fn a_log_with_no_marker_is_left_out() {
        let scratch =
            Scratch(std::env::temp_dir().join(format!("nanoprobe-nomark-{}", std::process::id())));
        let dir = scratch.0.clone();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Logs")).unwrap();
        std::fs::write(dir.join("Logs/dcs.log"), "2026 INFO APP: 203.0.113.77\n").unwrap();
        let bytes = build(
            &Local::new(&dir).unwrap(),
            &Facts::default(),
            KEY,
            Vec::new(),
        )
        .unwrap();

        let files = unzip(&bytes);
        assert_eq!(files.len(), 1);
        assert!(files[0].1.contains("no session marker"), "{}", files[0].1);
    }

    #[test]
    fn hashes_are_keyed_and_stable() {
        let a = Redactor::new(KEY, &[]).token("ip", "203.0.113.77");
        let b = Redactor::new(KEY, &[]).token("ip", "203.0.113.77");
        let c = Redactor::new(b"another key", &[]).token("ip", "203.0.113.77");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with("ip-") && a.len() == 11, "{a}");
    }
}
