//! Finding DCS Saved Games profiles, on Windows and inside a Wine prefix.
//!
//! A profile is a folder named `DCS` or `DCS.<branch>` in a `Saved Games`
//! folder. On Windows that is `%USERPROFILE%\Saved Games`. Under Wine it is
//! `<prefix>/drive_c/users/<user>/Saved Games` for every user in the prefix.
//! The environment is passed in, so a test names scratch folders and never
//! reaches the machine's own.

use std::path::{Path, PathBuf};

/// The environment values discovery reads.
#[derive(Clone, Debug, Default)]
pub struct Env {
    /// `USERPROFILE`, set on Windows.
    pub userprofile: Option<PathBuf>,
    /// `WINEPREFIX`, set when a Wine prefix other than `~/.wine` is in use.
    pub wineprefix: Option<PathBuf>,
    /// `HOME`.
    pub home: Option<PathBuf>,
}

impl Env {
    /// This process's environment.
    pub fn current() -> Env {
        let var = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Env {
            userprofile: var("USERPROFILE"),
            wineprefix: var("WINEPREFIX"),
            home: var("HOME"),
        }
    }
}

/// Where to look, from the command line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Where {
    /// `--profile`: this profile and no other.
    pub profile: Option<PathBuf>,
    /// `--wine-prefix`: profiles in this Wine prefix.
    pub wine_prefix: Option<PathBuf>,
}

/// The `Saved Games` folders to look in, each with the Wine prefix it is
/// in, if any.
fn saved_games(place: &Where, env: &Env) -> Vec<(PathBuf, Option<PathBuf>)> {
    let mut out = Vec::new();
    let wine = |prefix: &Path, out: &mut Vec<(PathBuf, Option<PathBuf>)>| {
        let users = prefix.join("drive_c").join("users");
        let Ok(entries) = std::fs::read_dir(&users) else {
            return;
        };
        let mut names: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path().join("Saved Games"))
            .filter(|p| p.is_dir())
            .collect();
        names.sort();
        out.extend(names.into_iter().map(|p| (p, Some(prefix.to_path_buf()))));
    };
    if let Some(prefix) = &place.wine_prefix {
        wine(prefix, &mut out);
        return out;
    }
    if let Some(user) = &env.userprofile {
        out.push((user.join("Saved Games"), None));
    }
    match (&env.wineprefix, &env.home) {
        (Some(prefix), _) => wine(prefix, &mut out),
        (None, Some(home)) => wine(&home.join(".wine"), &mut out),
        (None, None) => {}
    }
    out
}

/// One profile found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// The profile folder.
    pub path: PathBuf,
    /// The Wine prefix it is in, when it is in one.
    pub wine_prefix: Option<PathBuf>,
}

/// Whether `name` is a profile folder's name.
fn is_profile_name(name: &str) -> bool {
    name == "DCS"
        || name
            .strip_prefix("DCS.")
            .is_some_and(|rest| !rest.is_empty())
}

/// The Wine prefix `path` is in: the folder above `drive_c`.
pub fn wine_prefix_of(path: &Path) -> Option<PathBuf> {
    let mut at = path;
    while let Some(parent) = at.parent() {
        if at.file_name().is_some_and(|n| n == "drive_c") {
            return Some(parent.to_path_buf());
        }
        at = parent;
    }
    None
}

/// Every profile in the places to look, sorted by path. With `--profile`,
/// that folder alone, which must exist.
pub fn find(place: &Where, env: &Env) -> Result<Vec<Found>, String> {
    if let Some(profile) = &place.profile {
        if !profile.is_dir() {
            return Err(format!("no profile folder at {}", profile.display()));
        }
        return Ok(vec![Found {
            path: profile.clone(),
            wine_prefix: wine_prefix_of(profile),
        }]);
    }
    let mut out = Vec::new();
    for (folder, prefix) in saved_games(place, env) {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        let mut found: Vec<Found> = entries
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir())
            .filter(|e| is_profile_name(&e.file_name().to_string_lossy()))
            .map(|e| Found {
                path: e.path(),
                wine_prefix: prefix.clone(),
            })
            .collect();
        found.sort_by(|a, b| a.path.cmp(&b.path));
        out.extend(found);
    }
    Ok(out)
}

/// The folders searched, for a message saying nothing was found.
pub fn searched(place: &Where, env: &Env) -> String {
    if place.profile.is_some() {
        return String::new();
    }
    let mut places: Vec<String> = Vec::new();
    if let Some(prefix) = &place.wine_prefix {
        places.push(format!("{}/drive_c/users/*/Saved Games", prefix.display()));
    } else {
        if let Some(user) = &env.userprofile {
            places.push(user.join("Saved Games").display().to_string());
        }
        match (&env.wineprefix, &env.home) {
            (Some(prefix), _) => {
                places.push(format!("{}/drive_c/users/*/Saved Games", prefix.display()));
            }
            (None, Some(home)) => places.push(format!(
                "{}/drive_c/users/*/Saved Games",
                home.join(".wine").display()
            )),
            (None, None) => {}
        }
    }
    if places.is_empty() {
        "nowhere: neither USERPROFILE nor HOME is set".into()
    } else {
        places.join(", ")
    }
}

/// The one profile to install into or uninstall from: `--profile`, or the
/// only profile found.
pub fn one(place: &Where, env: &Env) -> Result<Found, String> {
    let mut found = find(place, env)?;
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(format!(
            "no DCS profile found in {}; name one with --profile or --wine-prefix",
            searched(place, env)
        )),
        _ => {
            let list: Vec<String> = found
                .iter()
                .map(|f| format!("  {}", f.path.display()))
                .collect();
            Err(format!(
                "more than one DCS profile; name one with --profile:\n{}",
                list.join("\n")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir()
                .join(format!("nanoprobe-profiles-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn mkdir(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
    }

    #[test]
    fn finds_windows_profiles_under_userprofile() {
        let s = Scratch::new("windows");
        let user = s.0.join("Users/pilot");
        mkdir(&user.join("Saved Games/DCS"));
        mkdir(&user.join("Saved Games/DCS.openbeta"));
        mkdir(&user.join("Saved Games/Other"));
        mkdir(&user.join("Saved Games/DCS."));
        let env = Env {
            userprofile: Some(user.clone()),
            ..Env::default()
        };
        let found = find(&Where::default(), &env).unwrap();
        let names: Vec<_> = found.iter().map(|f| f.path.clone()).collect();
        assert_eq!(
            names,
            [
                user.join("Saved Games/DCS"),
                user.join("Saved Games/DCS.openbeta")
            ]
        );
        assert!(found.iter().all(|f| f.wine_prefix.is_none()));
        assert!(
            one(&Where::default(), &env)
                .unwrap_err()
                .contains("more than one")
        );
    }

    #[test]
    fn finds_profiles_in_a_wine_prefix() {
        let s = Scratch::new("wine");
        let prefix = s.0.join("config/.wine");
        let profile = prefix.join("drive_c/users/abc/Saved Games/DCS.dcs_serverrelease");
        mkdir(&profile);
        mkdir(&prefix.join("drive_c/users/Public"));
        let place = Where {
            wine_prefix: Some(prefix.clone()),
            ..Where::default()
        };
        let found = one(&place, &Env::default()).unwrap();
        assert_eq!(found.path, profile);
        assert_eq!(found.wine_prefix.as_deref(), Some(prefix.as_path()));

        // The same prefix through WINEPREFIX, and as ~/.wine through HOME.
        let env = Env {
            wineprefix: Some(prefix.clone()),
            ..Env::default()
        };
        assert_eq!(one(&Where::default(), &env).unwrap().path, profile);
        let env = Env {
            home: Some(s.0.join("config")),
            ..Env::default()
        };
        assert_eq!(one(&Where::default(), &env).unwrap().path, profile);
    }

    #[test]
    fn an_explicit_profile_knows_its_prefix() {
        let s = Scratch::new("explicit");
        let profile = s.0.join("p/drive_c/users/abc/Saved Games/DCS");
        mkdir(&profile);
        let place = Where {
            profile: Some(profile.clone()),
            ..Where::default()
        };
        let found = one(&place, &Env::default()).unwrap();
        assert_eq!(found.wine_prefix, Some(s.0.join("p")));
        let missing = Where {
            profile: Some(s.0.join("none")),
            ..Where::default()
        };
        assert!(find(&missing, &Env::default()).is_err());
    }

    #[test]
    fn nothing_found_names_where_it_looked() {
        let s = Scratch::new("none");
        let env = Env {
            userprofile: Some(s.0.clone()),
            ..Env::default()
        };
        let error = one(&Where::default(), &env).unwrap_err();
        assert!(error.contains("Saved Games"), "{error}");
    }
}
