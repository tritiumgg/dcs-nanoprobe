//! The agent's settings, `<profile>/nanoprobe/agent.toml`, and the token
//! (spec/config.md).

use std::path::{Path, PathBuf};

use nanoprobe_transport::layout::CAPACITY;
use serde::Deserialize;

/// `agent.toml`. Every key has a default, so a missing file is every default.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AgentToml {
    /// `[transport]`
    pub transport: Transport,
    /// `[budget]`
    pub budget: Budget,
    /// `[sampling]`
    pub sampling: Sampling,
    /// `[lua]`
    pub lua: Lua,
}

/// `[transport]`
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Transport {
    /// The mapped file; empty means `nanoprobe-<port>.map` in the temp folder.
    pub map_path: String,
    /// The loopback TCP port of the handshake.
    pub port: u16,
    /// Outbound ring, KiB.
    pub out_ring_kib: u64,
    /// Inbound ring, KiB.
    pub in_ring_kib: u64,
    /// The event and log queue, KiB.
    pub event_queue_kib: u64,
    /// The control queue, in records.
    pub control_queue: u64,
}

impl Default for Transport {
    fn default() -> Self {
        Transport {
            map_path: String::new(),
            port: 47310,
            out_ring_kib: 4096,
            in_ring_kib: 1024,
            event_queue_kib: 1024,
            control_queue: 1024,
        }
    }
}

/// `[budget]`
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Budget {
    /// Agent overhead as a share of real time.
    pub overhead_share: f64,
    /// Commanded work per frame, milliseconds.
    pub commanded_ms: f64,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            overhead_share: 0.003,
            commanded_ms: 2.0,
        }
    }
}

/// `[sampling]`: the target period of each tier, seconds.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Sampling {
    /// Positions.
    pub position_s: f64,
    /// Life.
    pub vitals_s: f64,
    /// Warehouses and airbase owners.
    pub warehouse_s: f64,
    /// The full enumeration.
    pub enumeration_s: f64,
}

impl Default for Sampling {
    fn default() -> Self {
        Sampling {
            position_s: 1.0,
            vitals_s: 5.0,
            warehouse_s: 60.0,
            enumeration_s: 60.0,
        }
    }
}

/// `[lua]`
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Lua {
    /// Allow extensions, `EVAL` and `emit`.
    pub raw: bool,
}

impl AgentToml {
    /// Read `path`. A missing file is every default; a file that does not
    /// parse, has an unknown key, or breaks a rule is an error naming it.
    pub fn read(path: &Path) -> Result<AgentToml, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(AgentToml::default()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        AgentToml::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Parse and check the text of `agent.toml`.
    pub fn parse(text: &str) -> Result<AgentToml, String> {
        let t: AgentToml = toml::from_str(text).map_err(|e| e.to_string().trim().to_owned())?;
        t.check()?;
        Ok(t)
    }

    fn check(&self) -> Result<(), String> {
        let t = &self.transport;
        for (name, kib) in [
            ("out_ring_kib", t.out_ring_kib),
            ("in_ring_kib", t.in_ring_kib),
        ] {
            let bytes = kib.saturating_mul(1024);
            if !kib.is_power_of_two() || !CAPACITY.contains(&bytes) {
                return Err(format!(
                    "transport.{name} is {kib}: a power of two from 256 to 65536"
                ));
            }
        }
        if t.port == 0 {
            return Err("transport.port is 0".into());
        }
        if t.event_queue_kib == 0 || t.event_queue_kib > 1 << 20 {
            return Err(format!(
                "transport.event_queue_kib is {}: from 1 to 1048576",
                t.event_queue_kib
            ));
        }
        if t.control_queue == 0 || t.control_queue > 1 << 20 {
            return Err(format!(
                "transport.control_queue is {}: from 1 to 1048576",
                t.control_queue
            ));
        }
        let b = &self.budget;
        if !(b.overhead_share > 0.0 && b.overhead_share <= 1.0) {
            return Err(format!(
                "budget.overhead_share is {}: above 0, at most 1",
                b.overhead_share
            ));
        }
        if !(b.commanded_ms > 0.0 && b.commanded_ms <= 1000.0) {
            return Err(format!(
                "budget.commanded_ms is {}: above 0, at most 1000",
                b.commanded_ms
            ));
        }
        let s = &self.sampling;
        for (name, v) in [
            ("position_s", s.position_s),
            ("vitals_s", s.vitals_s),
            ("warehouse_s", s.warehouse_s),
            ("enumeration_s", s.enumeration_s),
        ] {
            if !(v > 0.0 && v.is_finite()) {
                return Err(format!("sampling.{name} is {v}: above 0"));
            }
        }
        Ok(())
    }

    /// The mapped file's path: `map_path`, or `nanoprobe-<port>.map` in the
    /// folder the operating system names for temporary files.
    pub fn map_path(&self) -> PathBuf {
        if self.transport.map_path.is_empty() {
            std::env::temp_dir().join(format!("nanoprobe-{}.map", self.transport.port))
        } else {
            PathBuf::from(&self.transport.map_path)
        }
    }
}

/// Read the token file: 64 lower-case hex digits, and nothing else but a line
/// ending. The error never holds the file's contents.
pub fn read_token(path: &Path) -> Result<String, String> {
    let text = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let trimmed = text
        .strip_suffix(b"\r\n")
        .or_else(|| text.strip_suffix(b"\n"))
        .unwrap_or(&text);
    if trimmed.len() != 64
        || !trimmed
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
    {
        return Err(format!("{}: not 64 lower-case hex digits", path.display()));
    }
    Ok(String::from_utf8_lossy(trimmed).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_is_every_default() {
        assert_eq!(AgentToml::parse("").unwrap(), AgentToml::default());
    }

    #[test]
    fn the_defaults_are_the_specified_ones() {
        let t = AgentToml::default();
        assert_eq!(t.transport.port, 47310);
        assert_eq!(t.transport.out_ring_kib, 4096);
        assert_eq!(t.transport.in_ring_kib, 1024);
        assert_eq!(t.transport.event_queue_kib, 1024);
        assert_eq!(t.transport.control_queue, 1024);
        assert_eq!(t.budget.overhead_share, 0.003);
        assert_eq!(t.budget.commanded_ms, 2.0);
        assert_eq!(t.sampling.position_s, 1.0);
        assert!(!t.lua.raw);
        assert!(t.map_path().ends_with("nanoprobe-47310.map"));
    }

    #[test]
    fn keys_are_read() {
        let t = AgentToml::parse(
            "[transport]\nmap_path = 'Z:\\dev\\shm\\n.map'\nport = 5000\nout_ring_kib = 256\n[lua]\nraw = true\n",
        )
        .unwrap();
        assert_eq!(t.transport.port, 5000);
        assert_eq!(t.transport.out_ring_kib, 256);
        assert_eq!(t.map_path(), PathBuf::from("Z:\\dev\\shm\\n.map"));
        assert!(t.lua.raw);
    }

    #[test]
    fn an_unknown_key_is_refused() {
        let e = AgentToml::parse("[transport]\nprot = 1\n").unwrap_err();
        assert!(e.contains("prot"), "{e}");
        assert!(AgentToml::parse("[nonsense]\n").is_err());
    }

    #[test]
    fn values_out_of_range_are_refused() {
        for text in [
            "[transport]\nout_ring_kib = 300\n",
            "[transport]\nin_ring_kib = 128\n",
            "[transport]\nin_ring_kib = 131072\n",
            "[transport]\nport = 0\n",
            "[transport]\ncontrol_queue = 0\n",
            "[budget]\noverhead_share = 0.0\n",
            "[budget]\ncommanded_ms = -1.0\n",
            "[budget]\ncommanded_ms = 1e300\n",
            "[sampling]\nwarehouse_s = 0.0\n",
        ] {
            assert!(AgentToml::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn a_token_must_be_64_lower_case_hex_digits() {
        /// Removes the folder when the test ends, whether it passes or fails.
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "nanoprobe-token-{}",
            nanoprobe_transport::random_nonzero()
        )));
        let dir = scratch.0.clone();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("token");
        let good = "0123456789abcdef".repeat(4);
        std::fs::write(&path, format!("{good}\n")).unwrap();
        assert_eq!(read_token(&path).unwrap(), good);
        std::fs::write(&path, good.to_uppercase()).unwrap();
        let e = read_token(&path).unwrap_err();
        assert!(
            !e.contains(&good.to_uppercase()),
            "the error holds the token"
        );
        std::fs::write(&path, &good[..63]).unwrap();
        assert!(read_token(&path).is_err());
        // A file that is gone is an error too.
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(read_token(&path).is_err());
    }
}
