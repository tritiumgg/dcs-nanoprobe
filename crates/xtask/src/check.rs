//! `target check`: read a target's facts without changing anything on it.
//!
//! One SSH session runs a POSIX script on the host. The script asks the
//! container for its Wine version, lists the Saved Games profiles under the
//! Wine prefix, and reads the version and branch from each DCS install's
//! `autoupdate.cfg`. It reads no environment variable and no server settings.

use std::fmt;

use crate::profile::Target;
use crate::ssh::{quote, script as ssh_script};

/// Runs on the host, with the container and the Wine prefix as `$1` and `$2`.
/// Prints `key=value` lines.
const SCRIPT: &str = r#"set -u
container=$1
prefix=$2
printf 'wine=%s\n' "$(docker exec "$container" wine --version 2>/dev/null)"
docker exec -i "$container" sh -s -- "$prefix" <<'INNER'
prefix=$1
for d in "$prefix"/drive_c/users/*/"Saved Games"/DCS*; do
    [ -d "$d" ] && printf 'profile=%s\n' "$d"
done
for i in "$prefix"/drive_c/"Program Files"/"Eagle Dynamics"/*; do
    f=$i/autoupdate.cfg
    [ -f "$f" ] || continue
    printf 'install=%s\n' "$i"
    sed -n -e 's/.*"version": *"\([^"]*\)".*/version=\1/p' \
           -e 's/.*"branch": *"\([^"]*\)".*/branch=\1/p' "$f"
done
INNER
"#;

/// What a check found.
#[derive(Debug, Default, PartialEq)]
pub struct Report {
    /// The output of `wine --version` in the container.
    pub wine: Option<String>,
    /// Each DCS install found.
    pub installs: Vec<Install>,
    /// Each Saved Games profile folder found.
    pub profiles: Vec<String>,
}

/// One DCS install and what its `autoupdate.cfg` says.
#[derive(Debug, Default, PartialEq)]
pub struct Install {
    /// The install folder.
    pub path: String,
    /// The DCS build, such as `2.9.31.29434`.
    pub version: Option<String>,
    /// The update branch, such as `dcs_server.release`.
    pub branch: Option<String>,
}

impl Report {
    /// Parse the script's `key=value` lines. A `version` or `branch` line
    /// belongs to the `install` line before it.
    pub fn parse(output: &str) -> Report {
        let mut report = Report::default();
        for line in output.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim().to_owned();
            match key {
                "wine" if !value.is_empty() => report.wine = Some(value),
                "profile" => report.profiles.push(value),
                "install" => report.installs.push(Install {
                    path: value,
                    ..Install::default()
                }),
                "version" => {
                    if let Some(install) = report.installs.last_mut() {
                        install.version = Some(value);
                    }
                }
                "branch" => {
                    if let Some(install) = report.installs.last_mut() {
                        install.branch = Some(value);
                    }
                }
                _ => {}
            }
        }
        report
    }

    /// Whether the check found Wine, a versioned install and a profile.
    pub fn is_complete(&self) -> bool {
        self.wine.is_some()
            && !self.profiles.is_empty()
            && self.installs.iter().any(|i| i.version.is_some())
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const MISSING: &str = "NOT FOUND";

        if self.installs.is_empty() {
            writeln!(f, "dcs       {MISSING}")?;
        }
        for install in &self.installs {
            writeln!(
                f,
                "dcs       {} ({})",
                install.version.as_deref().unwrap_or(MISSING),
                install.branch.as_deref().unwrap_or("unknown branch")
            )?;
            writeln!(f, "install   {}", install.path)?;
        }
        writeln!(f, "wine      {}", self.wine.as_deref().unwrap_or(MISSING))?;
        if self.profiles.is_empty() {
            writeln!(f, "profile   {MISSING}")?;
        }
        for profile in &self.profiles {
            writeln!(f, "profile   {profile}")?;
        }
        Ok(())
    }
}

/// Run the check against `target`.
pub fn run(target: &Target) -> Result<Report, String> {
    match target {
        Target::DockerWine {
            ssh,
            container,
            wine_prefix,
            ..
        } => {
            let remote = format!("sh -s -- {} {}", quote(container), quote(wine_prefix));
            let output = ssh_script(ssh, &remote, SCRIPT)?;
            Ok(Report::parse(&output))
        }
        Target::Local { .. } => Err("target check reads docker-wine targets only".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_report() {
        let report = Report::parse(
            "wine=wine-10.0\n\
             profile=/p/drive_c/users/u/Saved Games/DCS.dcs_serverrelease\n\
             install=/p/drive_c/Program Files/Eagle Dynamics/DCS World Server\n\
             branch=dcs_server.release\n\
             version=2.9.31.29434\n",
        );

        assert_eq!(report.wine.as_deref(), Some("wine-10.0"));
        assert_eq!(report.profiles.len(), 1);
        assert_eq!(report.installs[0].version.as_deref(), Some("2.9.31.29434"));
        assert_eq!(
            report.installs[0].branch.as_deref(),
            Some("dcs_server.release")
        );
        assert!(report.is_complete());
    }

    #[test]
    fn an_empty_wine_line_is_missing() {
        let report = Report::parse("wine=\n");
        assert_eq!(report.wine, None);
        assert!(!report.is_complete());
    }

    #[test]
    fn an_install_without_a_version_is_incomplete() {
        let report = Report::parse("wine=w\nprofile=p\ninstall=i\n");
        assert!(!report.is_complete());
        assert!(report.to_string().contains("NOT FOUND"));
    }
}
