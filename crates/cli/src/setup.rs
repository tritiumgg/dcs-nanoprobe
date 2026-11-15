//! `nanoprobe doctor`, `install` and `uninstall`: their options and the two
//! commands that change a profile.

use std::io::Write;
use std::path::PathBuf;

use nanoprobe_install::autoexec::{AUTOEXEC, Merged};
use nanoprobe_install::{Local, Payload, install, undeploy};

use crate::profiles::{self, Env, Where};

/// Options of `doctor`, `install` and `uninstall`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Where the profile is.
    pub place: Where,
    /// `--payload`: the folder holding what `install` installs. Without it,
    /// `payload/` beside the program.
    pub payload: Option<PathBuf>,
}

/// Parse the arguments after `doctor`, `install` or `uninstall`.
pub fn parse(sub: &str, args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_owned())),
            _ => (arg.as_str(), None),
        };
        let mut value = |name: &str| -> Result<PathBuf, String> {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .ok_or_else(|| format!("{name} needs a value"))
        };
        let slot = match flag {
            "--profile" => &mut options.place.profile,
            "--wine-prefix" => &mut options.place.wine_prefix,
            "--payload" if sub == "install" => &mut options.payload,
            _ => return Err(format!("unknown argument `{arg}` for `{sub}`")),
        };
        if slot.replace(value(flag)?).is_some() {
            return Err(format!("{flag} given twice"));
        }
    }
    if options.place.profile.is_some() && options.place.wine_prefix.is_some() {
        return Err("give --profile or --wine-prefix, not both".into());
    }
    Ok(options)
}

/// The payload folder: `--payload`, or `payload/` beside the program.
fn payload_dir(options: &Options) -> Result<PathBuf, String> {
    if let Some(dir) = &options.payload {
        return Ok(dir.clone());
    }
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this program: {e}"))?;
    let dir = exe.parent().map(|d| d.join("payload"));
    dir.ok_or_else(|| "cannot find this program's folder".into())
}

/// `nanoprobe install`. Returns the exit code.
pub fn run_install(options: &Options, env: &Env, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    match try_install(options, env, out) {
        Ok(()) => 0,
        Err(e) => {
            let _ = writeln!(err, "nanoprobe install: {e}");
            1
        }
    }
}

fn try_install(options: &Options, env: &Env, out: &mut dyn Write) -> Result<(), String> {
    let found = profiles::one(&options.place, env)?;
    let dir = payload_dir(options)?;
    let payload = Payload::from_package(&dir).map_err(|e| {
        format!("{e}; run the program from the unpacked release, or name its payload folder with --payload")
    })?;
    same_owner(&found.path)?;
    let profile = Local::new(&found.path)?;
    let installed = install(&profile, &payload)?;
    let shown = found.path.display();
    let d = &installed.deployed;
    let _ = writeln!(out, "profile {shown}");
    if d.switched {
        let _ = writeln!(out, "installed version {}", d.build);
    } else {
        let _ = writeln!(out, "version {} was already active", d.build);
    }
    if !d.pruned.is_empty() {
        let _ = writeln!(out, "removed old versions {}", d.pruned.join(", "));
    }
    if installed.configured.token_created {
        let _ = writeln!(out, "created the handshake token nanoprobe/token");
    }
    match installed.merged {
        Merged::Added => {
            let _ = writeln!(out, "added the nanoprobe block to {AUTOEXEC}");
        }
        Merged::Present => {}
    }
    let _ = writeln!(out, "restart DCS to load it, then run `nanoprobe doctor`");
    Ok(())
}

/// `nanoprobe uninstall`. Returns the exit code.
pub fn run_uninstall(
    options: &Options,
    env: &Env,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let result = profiles::one(&options.place, env)
        .and_then(|found| {
            same_owner(&found.path)?;
            Ok((Local::new(&found.path)?, found.path))
        })
        .and_then(|(profile, path)| {
            let removed = undeploy(&profile).map_err(|e| {
                // Windows does not delete a DLL a running process has loaded.
                format!("{e}; if DCS is running, quit it and run `nanoprobe uninstall` again")
            })?;
            Ok((removed, path))
        });
    match result {
        Ok((removed, path)) => {
            let _ = writeln!(out, "profile {}", path.display());
            if removed {
                let _ = writeln!(out, "removed nanoprobe");
            } else {
                let _ = writeln!(out, "nanoprobe was not installed");
            }
            0
        }
        Err(e) => {
            let _ = writeln!(err, "nanoprobe uninstall: {e}");
            1
        }
    }
}

/// Refuse to write into a profile another user owns. DCS runs as the
/// profile's owner, under Wine often in a container, and must read what
/// install writes, the owner-only token among it. Windows decides access by
/// the folder's access list, so it is not checked there.
#[cfg(unix)]
fn same_owner(profile: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(profile).map_err(|e| format!("{}: {e}", profile.display()))?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    owner_error(meta.uid(), me, profile)
}

#[cfg(not(unix))]
fn same_owner(_profile: &std::path::Path) -> Result<(), String> {
    Ok(())
}

/// The refusal when `me` is not `owner`.
#[cfg(any(unix, test))]
fn owner_error(owner: u32, me: u32, profile: &std::path::Path) -> Result<(), String> {
    if owner == me {
        return Ok(());
    }
    Err(format!(
        "{} belongs to user {owner} and this program runs as user {me}; DCS could not read what it writes. Run it as the profile's owner, for example with `sudo -u '#{owner}'`",
        profile.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(sub: &str, args: &[&str]) -> Result<Options, String> {
        parse(sub, &args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn refuses_another_users_profile() {
        let path = std::path::Path::new("p");
        assert!(owner_error(1000, 1000, path).is_ok());
        let error = owner_error(1000, 0, path).unwrap_err();
        assert!(error.contains("sudo -u '#1000'"), "{error}");
    }

    #[test]
    fn parses_the_places() {
        let o = p("install", &["--profile", "a", "--payload=b"]).unwrap();
        assert_eq!(o.place.profile, Some("a".into()));
        assert_eq!(o.payload, Some("b".into()));
        let o = p("doctor", &["--wine-prefix", "w"]).unwrap();
        assert_eq!(o.place.wine_prefix, Some("w".into()));
        for bad in [
            ("doctor", &["--payload", "b"][..]),
            ("doctor", &["--profile"]),
            ("doctor", &["--profile", "a", "--profile", "b"]),
            ("uninstall", &["--profile", "a", "--wine-prefix", "b"]),
            ("install", &["x"]),
        ] {
            assert!(p(bad.0, bad.1).is_err(), "{bad:?}");
        }
    }
}
