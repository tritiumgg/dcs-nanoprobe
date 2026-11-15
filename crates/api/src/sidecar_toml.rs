//! `sidecar.toml` (spec/config.md, "Sidecar"): the file the sidecar process
//! reads at start.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use nanoprobe_sidecar::link;
use nanoprobe_sidecar::sim::real::Limits;
use nanoprobe_sidecar::store::LostPolicy;
use serde::Deserialize;

use crate::config::{Config, StoreFiles};
use crate::real::Missions;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct File {
    agent: Agent,
    api: Api,
    store: Store,
    health: Health,
    world: World,
    log: Log,
    missions: MissionsToml,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct MissionsToml {
    dir: String,
    dcs_dir: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Agent {
    map_path: String,
    handshake: String,
    token_path: String,
    poll_ms: u64,
    command_queue: usize,
}

impl Default for Agent {
    fn default() -> Agent {
        Agent {
            map_path: String::new(),
            handshake: "127.0.0.1:47310".into(),
            token_path: String::new(),
            poll_ms: 2,
            command_queue: 4096,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Api {
    listen: String,
    tokens_path: String,
    tls_cert: String,
    tls_key: String,
    lease_ttl_s: u64,
    stream_buffer: usize,
    result_wait_s: u64,
}

impl Default for Api {
    fn default() -> Api {
        Api {
            listen: "127.0.0.1:47311".into(),
            tokens_path: String::new(),
            tls_cert: String::new(),
            tls_key: String::new(),
            lease_ttl_s: 30,
            stream_buffer: 1024,
            result_wait_s: 10,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Store {
    dir: String,
    enabled: bool,
    checkpoint_s: u64,
    backup_interval_h: u64,
    backup_keep: usize,
    event_retention_days: u64,
    server_log_days: u64,
    idempotency_keep_h: u64,
    world_changes_keep: usize,
}

impl Default for Store {
    fn default() -> Store {
        Store {
            dir: "campaigns".into(),
            enabled: true,
            checkpoint_s: 5,
            backup_interval_h: 24,
            backup_keep: 7,
            event_retention_days: 0,
            server_log_days: 30,
            idempotency_keep_h: 24,
            world_changes_keep: 100_000,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Health {
    heartbeat_stale_s: u64,
    loading_limit_s: u64,
    handshake_timeout_s: u64,
}

impl Default for Health {
    fn default() -> Health {
        Health {
            heartbeat_stale_s: 10,
            loading_limit_s: 900,
            handshake_timeout_s: 15,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct World {
    position_period_min_s: f64,
    materialize_per_frame: usize,
    on_lost: String,
    destroy_retries: u32,
}

impl Default for World {
    fn default() -> World {
        World {
            position_period_min_s: 0.1,
            materialize_per_frame: 1,
            on_lost: "report".into(),
            destroy_retries: 3,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Log {
    dir: String,
    level: String,
}

impl Default for Log {
    fn default() -> Log {
        Log {
            dir: "logs".into(),
            level: "info".into(),
        }
    }
}

/// Everything `sidecar.toml` sets.
#[derive(Clone, Debug)]
pub struct SidecarToml {
    /// The API server's settings.
    pub api: Config,
    /// The link to the agent.
    pub link: link::Config,
    /// The health verdict's limits.
    pub limits: Limits,
    /// `[world] on_lost`.
    pub on_lost: LostPolicy,
    /// `[missions]`.
    pub missions: Missions,
}

impl SidecarToml {
    /// Read and check the file at `path`.
    pub fn read(path: &Path) -> Result<SidecarToml, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let base = path.parent().unwrap_or(Path::new("."));
        SidecarToml::parse(&text, base).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Parse `text`; relative paths are relative to `base`.
    pub fn parse(text: &str, base: &Path) -> Result<SidecarToml, String> {
        let f: File = toml::from_str(text).map_err(|e| e.to_string())?;
        let rel = |p: &str| -> PathBuf {
            let p = PathBuf::from(p);
            if p.is_absolute() { p } else { base.join(p) }
        };
        if f.agent.map_path.is_empty() {
            return Err("[agent] map_path is required".into());
        }
        if f.agent.token_path.is_empty() {
            return Err("[agent] token_path is required".into());
        }
        if !(1..=5).contains(&f.agent.poll_ms) {
            return Err("[agent] poll_ms is 1 to 5".into());
        }
        if !f.api.tls_cert.is_empty() || !f.api.tls_key.is_empty() {
            return Err(
                "[api] tls_cert and tls_key: TLS is not built yet; leave them empty".into(),
            );
        }
        if f.world.destroy_retries < 1 {
            return Err("[world] destroy_retries is at least 1".into());
        }
        if f.world.position_period_min_s.is_nan() || f.world.position_period_min_s <= 0.0 {
            return Err("[world] position_period_min_s is above 0".into());
        }
        let on_lost = match f.world.on_lost.as_str() {
            "report" => LostPolicy::Report,
            "respawn" => LostPolicy::Respawn,
            "dead" => LostPolicy::Dead,
            other => return Err(format!("[world] on_lost: unknown policy `{other}`")),
        };
        let handshake: SocketAddr = f
            .agent
            .handshake
            .parse()
            .map_err(|e| format!("[agent] handshake: {e}"))?;
        let listen: SocketAddr = f
            .api
            .listen
            .parse()
            .map_err(|e| format!("[api] listen: {e}"))?;
        let missions = Missions {
            dir: (!f.missions.dir.is_empty()).then(|| rel(&f.missions.dir)),
            dcs_dir: f.missions.dcs_dir,
        };
        missions.check()?;
        let token_path = rel(&f.agent.token_path);
        let token = std::fs::read_to_string(&token_path)
            .map_err(|e| format!("[agent] token_path {}: {e}", token_path.display()))?
            .trim()
            .to_owned();
        let hours = |h: u64| Duration::from_secs(h * 3600);
        let days = |d: u64| hours(d * 24);
        let store = f.store.enabled.then(|| StoreFiles {
            dir: rel(&f.store.dir),
            checkpoint: Duration::from_secs(f.store.checkpoint_s.max(1)),
            backup_interval: hours(f.store.backup_interval_h),
            backup_keep: f.store.backup_keep,
            event_retention: days(f.store.event_retention_days),
            server_log_retention: days(f.store.server_log_days),
        });
        let poll = Duration::from_millis(f.agent.poll_ms);
        let api = Config {
            listen,
            tokens_path: (!f.api.tokens_path.is_empty()).then(|| rel(&f.api.tokens_path)),
            lease_ttl: Duration::from_secs(f.api.lease_ttl_s),
            stream_buffer: f.api.stream_buffer,
            result_wait: Duration::from_secs(f.api.result_wait_s),
            idempotency_keep: hours(f.store.idempotency_keep_h),
            world_changes_keep: f.store.world_changes_keep,
            position_period_min: Duration::from_secs_f64(f.world.position_period_min_s),
            materialize_per_frame: f.world.materialize_per_frame.max(1),
            destroy_retries: f.world.destroy_retries,
            on_lost,
            step: poll,
            store,
            ..Config::default()
        };
        let _ = (f.agent.command_queue, f.log.dir, f.log.level);
        Ok(SidecarToml {
            api,
            link: link::Config {
                map_path: rel(&f.agent.map_path),
                handshake,
                token,
                poll,
                handshake_timeout: Duration::from_secs(f.health.handshake_timeout_s),
            },
            limits: Limits {
                heartbeat_stale: Duration::from_secs(f.health.heartbeat_stale_s),
                loading_limit: Duration::from_secs(f.health.loading_limit_s),
            },
            on_lost,
            missions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deployed_sidecar_toml_is_accepted() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let text = std::fs::read_to_string(root.join("deploy/sidecar.toml")).expect("read");
        let d = dir_with_token();
        let token = d.path().join("token");
        let text = text
            .replace("/etc/nanoprobe/agent-token", token.to_str().expect("utf-8"))
            .replace("<Windows path of the DCS profile>", "Z:\\p");
        let t = SidecarToml::parse(&text, d.path()).expect("parse");
        assert_eq!(t.api.listen, "127.0.0.1:47311".parse().unwrap());
        assert_eq!(t.missions.dir, Some(PathBuf::from("/missions")));
    }

    fn dir_with_token() -> tempfile::TempDir {
        let d = tempfile::tempdir().expect("dir");
        std::fs::write(d.path().join("token"), "ab".repeat(32) + "\n").expect("token");
        d
    }

    #[test]
    fn the_defaults_are_the_spec_s() {
        let d = dir_with_token();
        let t = SidecarToml::parse(
            "[agent]\nmap_path = \"/dev/shm/nanoprobe.map\"\ntoken_path = \"token\"\n",
            d.path(),
        )
        .expect("parse");
        assert_eq!(t.link.token, "ab".repeat(32));
        assert_eq!(t.link.handshake, "127.0.0.1:47310".parse().unwrap());
        assert_eq!(t.api.listen, "127.0.0.1:47311".parse().unwrap());
        assert_eq!(t.link.poll, Duration::from_millis(2));
        assert_eq!(t.link.handshake_timeout, Duration::from_secs(15));
        assert_eq!(t.limits.heartbeat_stale, Duration::from_secs(10));
        assert_eq!(t.limits.loading_limit, Duration::from_secs(900));
        assert_eq!(
            t.api.store.as_ref().map(|s| s.dir.clone()),
            Some(d.path().join("campaigns"))
        );
        assert_eq!(t.on_lost, LostPolicy::Report);
        assert_eq!(t.api.on_lost, LostPolicy::Report);
        let r = SidecarToml::parse(
            "[agent]\nmap_path = \"m\"\ntoken_path = \"token\"\n[world]\non_lost = \"respawn\"\n",
            d.path(),
        )
        .expect("parse");
        assert_eq!(r.api.on_lost, LostPolicy::Respawn);
    }

    #[test]
    fn bad_files_are_refused() {
        let d = dir_with_token();
        let ok = "[agent]\nmap_path = \"m\"\ntoken_path = \"token\"\n";
        for (text, why) in [
            ("[agent]\ntoken_path = \"token\"\n", "map_path"),
            ("[agent]\nmap_path = \"m\"\n", "token_path"),
            (&format!("{ok}poll_ms = 9\n"), "poll_ms"),
            (&format!("{ok}nonsense = 1\n"), "unknown field"),
            (&format!("{ok}[world]\non_lost = \"keep\"\n"), "on_lost"),
            (&format!("{ok}[api]\ntls_cert = \"c\"\n"), "TLS"),
            (
                "[agent]\nmap_path = \"m\"\ntoken_path = \"missing\"\n",
                "missing",
            ),
            (&format!("{ok}[missions]\ntheaters = 1\n"), "unknown field"),
            (
                &format!("{ok}[missions]\ndcs_dir = \"{}\"\n", "x".repeat(1000)),
                "1024",
            ),
        ] {
            let e = SidecarToml::parse(text, d.path()).expect_err(why);
            assert!(e.contains(why), "{why}: {e}");
        }
    }

    #[test]
    fn a_disabled_store_is_in_memory() {
        let d = dir_with_token();
        let t = SidecarToml::parse(
            "[agent]\nmap_path = \"m\"\ntoken_path = \"token\"\n[store]\nenabled = false\n",
            d.path(),
        )
        .expect("parse");
        assert!(t.api.store.is_none());
    }
}
