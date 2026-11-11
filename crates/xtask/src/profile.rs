//! Target profiles: where a DCS server is and how to reach it.
//!
//! Profiles hold hosts, container names and paths, so they live outside the
//! repository, in a TOML file:
//!
//! ```toml
//! [targets.test-server]
//! kind = "docker-wine"          # DCS dedicated server under Wine in Docker
//! ssh = "my-host"               # ssh destination; ~/.ssh/config does the rest
//! container = "dcs"             # the container DCS runs in
//! wine_prefix = "/config/.wine" # the Wine prefix inside the container
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

/// One target, by kind.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Target {
    /// A DCS dedicated server under Wine in a Docker container, reached over SSH.
    DockerWine {
        /// The SSH destination: a host, `user@host`, or an alias.
        ssh: String,
        /// The container DCS runs in.
        container: String,
        /// The Wine prefix inside the container.
        wine_prefix: String,
    },
}

impl Target {
    /// A one-line description, for listings.
    pub fn describe(&self) -> String {
        match self {
            Target::DockerWine { ssh, container, .. } => {
                format!("docker-wine, container {container} on {ssh}")
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    targets: BTreeMap<String, Target>,
}

/// Parse a profiles file's text.
pub fn parse(text: &str) -> Result<BTreeMap<String, Target>, String> {
    toml::from_str::<File>(text)
        .map(|file| file.targets)
        .map_err(|e| e.to_string())
}

/// Read the profiles file, and return its path with the targets it holds.
pub fn load() -> Result<(PathBuf, BTreeMap<String, Target>), String> {
    let path = path()?;
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "could not read {}: {e}\nCreate it with a [targets.<name>] table; \
             see `cargo xtask help`.",
            path.display()
        )
    })?;
    let targets = parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((path, targets))
}

/// The profiles file: `NANOPROBE_TARGETS`, else the per-user config folder.
fn path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("NANOPROBE_TARGETS") {
        return Ok(PathBuf::from(path));
    }

    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    };

    base.map(|dir| dir.join("nanoprobe").join("targets.toml"))
        .ok_or_else(|| "no config folder: set NANOPROBE_TARGETS".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_docker_wine_target() {
        let targets = parse(
            r#"
            [targets.server]
            kind = "docker-wine"
            ssh = "host"
            container = "dcs"
            wine_prefix = "/config/.wine"
            "#,
        )
        .unwrap();

        assert_eq!(
            targets["server"],
            Target::DockerWine {
                ssh: "host".into(),
                container: "dcs".into(),
                wine_prefix: "/config/.wine".into(),
            }
        );
    }

    #[test]
    fn an_empty_file_has_no_targets() {
        assert!(parse("").unwrap().is_empty());
    }

    #[test]
    fn refuses_an_unknown_kind() {
        let error = parse("[targets.x]\nkind = \"vax\"\n").unwrap_err();
        assert!(error.contains("vax"), "{error}");
    }

    #[test]
    fn refuses_an_unknown_key() {
        let error = parse(
            r#"
            [targets.server]
            kind = "docker-wine"
            ssh = "host"
            container = "dcs"
            wine_prefix = "/config/.wine"
            password = "no"
            "#,
        )
        .unwrap_err();
        assert!(error.contains("password"), "{error}");
    }
}
