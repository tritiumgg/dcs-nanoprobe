//! Command-line parsing.

use std::path::PathBuf;

/// The help text.
pub const USAGE: &str = "\
Usage:
  nanoprobe inspect <map-file> [--records N]
  nanoprobe top <map-file> [--interval-ms MS] [--once]
  nanoprobe watch world|events [--addr URL] [--campaign ID] [--since N]
  nanoprobe doctor [--profile DIR | --wine-prefix DIR]
  nanoprobe install [--profile DIR | --wine-prefix DIR] [--payload DIR]
  nanoprobe uninstall [--profile DIR | --wine-prefix DIR]
  nanoprobe restore --store DIR --campaign ID --backup N

inspect  Print the status block, the metrics and the unread records of both
         rings. --records N lists at most N records per ring (default 50).
top      Show the live state and its rates. --interval-ms MS sets the refresh
         period (default 500). --once prints one screen and exits. Press
         Ctrl-C to stop.

watch    Print a sidecar's world changes or events as they arrive, one
         message per line. --addr URL is the API (default NANOPROBE_ADDR, or
         http://127.0.0.1:47311); a token is read from NANOPROBE_TOKEN.
         --campaign ID reads that campaign (default the active one). --since N
         starts after store revision N (world, default: list first, then watch
         from the listed revision) or after event sequence N (events, default:
         the oldest kept).

doctor     Check an install from outside DCS: the files, the token and
           settings, the platform's lines in Logs/dcs.log, the DCS names the
           hook and the shim found, and the net.* keys in
           Config/autoexec.cfg. Writes nothing. Without --profile it checks
           every DCS Saved Games profile that holds nanoprobe: under
           USERPROFILE on Windows, and in the Wine prefix (--wine-prefix,
           WINEPREFIX, or ~/.wine). Exit 1 when a check fails, 2 when no
           installed profile is found.
install    Install the release into one DCS Saved Games profile: the loader,
           the agent and its Lua, a handshake token, and a block in
           Config/autoexec.cfg that lets the hook reach the server state.
           Nothing goes into the DCS install. --payload DIR names the
           release's payload folder (default: payload/ beside this program).
uninstall  Remove everything install added, the autoexec.cfg block included.

restore  Restore backup N of a campaign in the sidecar's store dir, with the
         sidecar stopped: the store it replaces is kept in the campaign's
         folder as restore-<time>/store.sqlite, entity IDs and revisions
         stay above both copies', and the campaign is suspended unless the
         backup is finished. The events file is not rolled back.

inspect and top open the file read-only.
Exit codes: 0 success, 2 usage error, 1 the file cannot be opened, the
stream fails, a doctor check fails, or install, uninstall or restore fails.
";

/// Records listed per ring when `--records` is absent.
pub const DEFAULT_RECORDS: usize = 50;
/// Refresh period of `top` when `--interval-ms` is absent.
pub const DEFAULT_INTERVAL_MS: u64 = 500;

/// A parsed command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Print the help text.
    Help,
    /// `nanoprobe inspect`.
    Inspect {
        /// The mapped file.
        path: PathBuf,
        /// Records to list per ring.
        records: usize,
    },
    /// `nanoprobe top`.
    Top {
        /// The mapped file.
        path: PathBuf,
        /// Milliseconds between refreshes.
        interval_ms: u64,
        /// Print one screen without clearing, then exit.
        once: bool,
    },
    /// `nanoprobe watch`.
    Watch(crate::watch::Options),
    /// `nanoprobe doctor`.
    Doctor(crate::setup::Options),
    /// `nanoprobe install`.
    Install(crate::setup::Options),
    /// `nanoprobe uninstall`.
    Uninstall(crate::setup::Options),
    /// `nanoprobe restore`.
    Restore(crate::restore::Options),
}

/// Parse the arguments after the program name. The error is a one-line message.
pub fn parse(args: &[String]) -> Result<Command, String> {
    let mut it = args.iter();
    let Some(sub) = it.next() else {
        return Err("a subcommand is required".into());
    };
    if matches!(sub.as_str(), "-h" | "--help" | "help") {
        return Ok(Command::Help);
    }
    if sub == "watch" {
        return crate::watch::parse(&args[1..]).map(Command::Watch);
    }
    if sub == "restore" {
        return crate::restore::parse(&args[1..]).map(Command::Restore);
    }
    let setup =
        |f: fn(crate::setup::Options) -> Command| crate::setup::parse(sub, &args[1..]).map(f);
    match sub.as_str() {
        "doctor" => return setup(Command::Doctor),
        "install" => return setup(Command::Install),
        "uninstall" => return setup(Command::Uninstall),
        _ => {}
    }
    if sub != "inspect" && sub != "top" {
        return Err(format!("unknown subcommand `{sub}`"));
    }
    let mut path: Option<PathBuf> = None;
    let mut records = DEFAULT_RECORDS;
    let mut interval_ms = DEFAULT_INTERVAL_MS;
    let mut once = false;
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            match inline.clone().or_else(|| it.next().cloned()) {
                Some(v) => Ok(v),
                None => Err(format!("{name} needs a value")),
            }
        };
        match flag {
            "--records" if sub == "inspect" => {
                records = value("--records")?
                    .parse()
                    .map_err(|_| "--records needs a whole number".to_string())?;
            }
            "--interval-ms" if sub == "top" => {
                interval_ms = value("--interval-ms")?
                    .parse()
                    .ok()
                    .filter(|&n| n > 0)
                    .ok_or_else(|| "--interval-ms needs a number above 0".to_string())?;
            }
            "--once" if sub == "top" && inline.is_none() => once = true,
            f if f.starts_with('-') && f.len() > 1 => {
                return Err(format!("unknown option `{f}` for `{sub}`"));
            }
            _ => {
                if path.replace(PathBuf::from(arg)).is_some() {
                    return Err("more than one map file".into());
                }
            }
        }
    }
    let Some(path) = path else {
        return Err("a map file is required".into());
    };
    Ok(if sub == "inspect" {
        Command::Inspect { path, records }
    } else {
        Command::Top {
            path,
            interval_ms,
            once,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Command, String> {
        parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn inspect_and_top_parse() {
        assert_eq!(
            p(&["inspect", "f", "--records", "7"]),
            Ok(Command::Inspect {
                path: "f".into(),
                records: 7
            })
        );
        assert_eq!(
            p(&["top", "--once", "f", "--interval-ms=100"]),
            Ok(Command::Top {
                path: "f".into(),
                interval_ms: 100,
                once: true
            })
        );
        assert_eq!(p(&["--help"]), Ok(Command::Help));
    }

    #[test]
    fn bad_lines_are_usage_errors() {
        for bad in [
            &[][..],
            &["frob"],
            &["inspect"],
            &["inspect", "a", "b"],
            &["inspect", "a", "--records"],
            &["inspect", "a", "--records", "x"],
            &["inspect", "a", "--once"],
            &["top", "a", "--interval-ms", "0"],
            &["top", "a", "--bogus"],
        ] {
            assert!(p(bad).is_err(), "{bad:?}");
        }
    }
}
