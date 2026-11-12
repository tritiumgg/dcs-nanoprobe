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
//! # The Saved Games profile to install into, as `target check` prints it.
//! profile = "/config/.wine/drive_c/users/u/Saved Games/DCS.server"
//! restart = "docker restart dcs" # run on the host to restart DCS
//!
//! [targets.this-machine]
//! kind = "local"                # a profile on this machine's file system
//! profile = "/path/to/Saved Games/DCS"
//!
//! [targets.windows-pc]
//! kind = "windows-ssh"          # DCS on Windows, reached over SSH
//! ssh = "my-pc"                 # ssh destination; the account's shell is PowerShell
//! profile = 'C:\Users\u\Saved Games\DCS'
//! install = 'C:\Program Files\Eagle Dynamics\DCS World' # optional; this is the default
//! start_task = '\nanoprobe\nanoprobe-start-dcs' # scheduled task that starts DCS
//! stop_task = '\nanoprobe\nanoprobe-stop-dcs'   # scheduled task that ends it
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
        /// The Saved Games profile inside the container. Deploy needs it.
        #[serde(default)]
        profile: Option<String>,
        /// The command, run on the host, that restarts DCS, such as
        /// `docker restart dcs`. `cargo xtask restart` needs it.
        #[serde(default)]
        restart: Option<String>,
    },
    /// DCS on a Windows machine reached over SSH. DCS runs in the desktop
    /// user's session, started and ended by two scheduled tasks the SSH
    /// account may run.
    WindowsSsh {
        /// The SSH destination: a host, `user@host`, or an alias.
        ssh: String,
        /// The Saved Games profile, as a Windows path.
        profile: String,
        /// The DCS install folder, read for its version.
        #[serde(default)]
        install: Option<String>,
        /// The scheduled task that starts DCS.
        start_task: String,
        /// The scheduled task that ends DCS.
        stop_task: String,
    },
    /// A Saved Games profile on this machine.
    Local {
        /// The profile folder.
        profile: String,
    },
}

impl Target {
    /// A one-line description, for listings.
    pub fn describe(&self) -> String {
        match self {
            Target::DockerWine { ssh, container, .. } => {
                format!("docker-wine, container {container} on {ssh}")
            }
            Target::WindowsSsh { ssh, profile, .. } => format!("windows-ssh, {profile} on {ssh}"),
            Target::Local { profile } => format!("local, {profile}"),
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
    Ok(config_dir()?.join("targets.toml"))
}

/// The per-user config folder: `~/.config/nanoprobe`, or
/// `%APPDATA%\nanoprobe` on Windows. It holds what stays off the repository.
pub fn config_dir() -> Result<PathBuf, String> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    };

    base.map(|dir| dir.join("nanoprobe"))
        .ok_or_else(|| "no config folder: set HOME, or APPDATA on Windows".to_owned())
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
                profile: None,
                restart: None,
            }
        );
    }

    #[test]
    fn reads_a_local_target() {
        let targets = parse("[targets.here]\nkind = \"local\"\nprofile = \"/p\"\n").unwrap();
        assert_eq!(
            targets["here"],
            Target::Local {
                profile: "/p".into()
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
