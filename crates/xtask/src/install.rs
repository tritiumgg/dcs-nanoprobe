//! Deploy, rollback and undeploy, as `spec/install.md` lays them out.
//!
//! The logic runs against [`Profile`], a small file-system interface rooted at
//! a Saved Games profile, so the same code serves a local folder and a
//! container over SSH. Paths given to a [`Profile`] are relative and use `/`.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The loader's path in the profile.
pub const LOADER: &str = "Scripts/Hooks/nanoprobe-hook.lua";
/// The platform's folder in the profile.
pub const ROOT: &str = "nanoprobe";
const MANIFEST: &str = "nanoprobe/manifest.txt";
const RECORD: &str = "nanoprobe/install.txt";
const LOGS: &str = "nanoprobe/logs";
const VERSIONS: &str = "nanoprobe/versions";
/// The loader's parent folders, outermost first.
const LOADER_DIRS: [&str; 2] = ["Scripts", "Scripts/Hooks"];

/// File operations on one Saved Games profile.
pub trait Profile {
    /// A file's bytes, or None when it does not exist.
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, String>;
    /// Write a file whole, creating its parent folders. Readers see the old
    /// file or the new one, never a part.
    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), String>;
    /// Create a folder and its parents.
    fn create_dir(&self, path: &str) -> Result<(), String>;
    /// Whether a folder exists.
    fn is_dir(&self, path: &str) -> Result<bool, String>;
    /// The names in a folder, or nothing when it does not exist.
    fn list(&self, path: &str) -> Result<Vec<String>, String>;
    /// Remove a file; a missing file is not an error.
    fn remove_file(&self, path: &str) -> Result<(), String>;
    /// Remove a folder and everything in it; a missing folder is not an error.
    fn remove_tree(&self, path: &str) -> Result<(), String>;
    /// Remove a folder if it is empty; otherwise leave it.
    fn remove_empty_dir(&self, path: &str) -> Result<(), String>;
    /// A file's SHA-256 in lower-case hex, or None when it does not exist.
    fn sha256(&self, path: &str) -> Result<Option<String>, String>;
    /// A text file from the last line holding both `marker` strings to its
    /// end, or None when the file or the line does not exist. A remote
    /// profile cuts the text on the target, so earlier lines never leave it.
    fn read_from_last(&self, path: &str, marker: [&str; 2]) -> Result<Option<Vec<u8>>, String>;
}

/// `text` from the last line holding both `marker` strings, or None.
pub fn from_last(text: &[u8], marker: [&str; 2]) -> Option<Vec<u8>> {
    let text = String::from_utf8_lossy(text);
    let mut start = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if line.contains(marker[0]) && line.contains(marker[1]) {
            start = Some(offset);
        }
        offset += line.len();
    }
    start.map(|start| text[start..].as_bytes().to_vec())
}

/// What one version installs.
#[derive(Debug, Clone)]
pub struct Payload {
    /// The loader, `Scripts/Hooks/nanoprobe-hook.lua`.
    pub loader: Vec<u8>,
    /// The agent DLL.
    pub dll: Vec<u8>,
    /// Files under the version folder other than the DLL, by relative path.
    pub files: Vec<(String, Vec<u8>)>,
}

impl Payload {
    /// The payload from this repository's Lua files and a built DLL.
    pub fn from_repository(repo: &Path, dll: &Path) -> Result<Payload, String> {
        let read = |path: PathBuf| {
            std::fs::read(&path).map_err(|e| format!("could not read {}: {e}", path.display()))
        };
        Ok(Payload {
            loader: read(repo.join("lua/hook/nanoprobe-hook.lua"))?,
            dll: read(dll.to_path_buf())?,
            files: vec![
                (
                    "lua/hook.lua".into(),
                    read(repo.join("lua/agent/hook.lua"))?,
                ),
                ("lua/log.lua".into(), read(repo.join("lua/agent/log.lua"))?),
            ],
        })
    }

    /// The build: 16 hex digits of the SHA-256 of the checksum list, with the
    /// DLL listed under the fixed name `agent.dll`, since its real name holds
    /// the build.
    pub fn build(&self) -> String {
        let list = self.checksums("agent.dll");
        hex(&Sha256::digest(list.as_bytes()))[..16].to_owned()
    }

    /// The DLL's file name in the version folder.
    pub fn dll_name(&self) -> String {
        format!("nanoprobe_agent_{}.dll", self.build())
    }

    /// Every file in the version folder, `SHA256SUMS` aside, by relative path.
    fn version_files(&self) -> Vec<(String, &[u8])> {
        let mut files: Vec<(String, &[u8])> = vec![(self.dll_name(), &self.dll)];
        for (path, bytes) in &self.files {
            files.push((path.clone(), bytes));
        }
        files.sort();
        files
    }

    /// `SHA256SUMS` text: `<sha256>  <path>` lines in path order.
    fn checksums(&self, dll_name: &str) -> String {
        let mut lines: Vec<(String, String)> = vec![(dll_name.to_owned(), sha256(&self.dll))];
        for (path, bytes) in &self.files {
            lines.push((path.clone(), sha256(bytes)));
        }
        lines.sort();
        let mut out = String::new();
        for (path, sum) in lines {
            let _ = writeln!(out, "{sum}  {path}");
        }
        out
    }
}

/// The manifest's two keys.
#[derive(Debug, Default, PartialEq, Eq, Clone)]
pub struct Manifest {
    /// The version the loader runs.
    pub active: Option<String>,
    /// The version rollback returns to.
    pub previous: Option<String>,
}

impl Manifest {
    fn parse(text: &str) -> Manifest {
        let mut manifest = Manifest::default();
        for line in text.lines() {
            match line.trim().split_once('=') {
                Some(("active", v)) if is_build(v) => manifest.active = Some(v.to_owned()),
                Some(("previous", v)) if is_build(v) => manifest.previous = Some(v.to_owned()),
                _ => {}
            }
        }
        manifest
    }

    fn render(&self) -> String {
        let mut out = String::new();
        if let Some(active) = &self.active {
            let _ = writeln!(out, "active={active}");
        }
        if let Some(previous) = &self.previous {
            let _ = writeln!(out, "previous={previous}");
        }
        out
    }
}

fn is_build(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Read the manifest; absent is empty.
pub fn manifest(profile: &dyn Profile) -> Result<Manifest, String> {
    Ok(profile
        .read(MANIFEST)?
        .map(|bytes| Manifest::parse(&String::from_utf8_lossy(&bytes)))
        .unwrap_or_default())
}

/// What a deploy did.
#[derive(Debug, PartialEq, Eq)]
pub struct Deployed {
    /// The build now active.
    pub build: String,
    /// Whether the version folder was written, or was already in place.
    pub wrote_version: bool,
    /// Whether the manifest changed.
    pub switched: bool,
    /// Version folders removed because no manifest key names them.
    pub pruned: Vec<String>,
}

/// Install `payload` and make it active.
pub fn deploy(profile: &dyn Profile, payload: &Payload) -> Result<Deployed, String> {
    let build = payload.build();

    // Record the loader folders that do not exist yet, before creating any,
    // so a deploy that stops halfway can still be undeployed.
    if profile.read(RECORD)?.is_none() {
        let mut created = String::new();
        for dir in LOADER_DIRS {
            if !profile.is_dir(dir)? {
                let _ = writeln!(created, "{dir}");
            }
        }
        profile.write(RECORD, created.as_bytes())?;
    }

    let version = format!("{VERSIONS}/{build}");
    let sums = payload.checksums(&payload.dll_name());
    let wrote_version = !version_intact(profile, &version, &sums)?;
    if wrote_version {
        profile.remove_tree(&version)?;
        for (path, bytes) in payload.version_files() {
            profile.write(&format!("{version}/{path}"), bytes)?;
        }
        profile.write(&format!("{version}/SHA256SUMS"), sums.as_bytes())?;
        if !version_intact(profile, &version, &sums)? {
            return Err(format!("{version}: checksums do not match after upload"));
        }
    }

    profile.create_dir(LOGS)?;
    if profile.read(LOADER)?.as_deref() != Some(&payload.loader[..]) {
        profile.write(LOADER, &payload.loader)?;
    }

    let old = manifest(profile)?;
    let switched = old.active.as_deref() != Some(&build);
    let new = if switched {
        Manifest {
            active: Some(build.clone()),
            previous: old.active.clone(),
        }
    } else {
        old
    };
    if switched {
        profile.write(MANIFEST, new.render().as_bytes())?;
    }

    let pruned = prune(profile, &new)?;
    Ok(Deployed {
        build,
        wrote_version,
        switched,
        pruned,
    })
}

/// Whether `version` holds exactly the files `sums` lists, with those sums.
fn version_intact(profile: &dyn Profile, version: &str, sums: &str) -> Result<bool, String> {
    if profile.read(&format!("{version}/SHA256SUMS"))?.as_deref() != Some(sums.as_bytes()) {
        return Ok(false);
    }
    for line in sums.lines() {
        let Some((sum, path)) = line.split_once("  ") else {
            return Ok(false);
        };
        if profile.sha256(&format!("{version}/{path}"))?.as_deref() != Some(sum) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Remove every version folder the manifest does not name.
fn prune(profile: &dyn Profile, manifest: &Manifest) -> Result<Vec<String>, String> {
    let keep: BTreeSet<&str> = [manifest.active.as_deref(), manifest.previous.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    let mut pruned = Vec::new();
    for name in profile.list(VERSIONS)? {
        if !keep.contains(name.as_str()) {
            profile.remove_tree(&format!("{VERSIONS}/{name}"))?;
            pruned.push(name);
        }
    }
    Ok(pruned)
}

/// Swap `active` and `previous`. Returns the manifest written.
pub fn rollback(profile: &dyn Profile) -> Result<Manifest, String> {
    let old = manifest(profile)?;
    let (Some(active), Some(previous)) = (old.active, old.previous) else {
        return Err("nothing to roll back to: the manifest names no previous version".into());
    };
    if !profile.is_dir(&format!("{VERSIONS}/{previous}"))? {
        return Err(format!("the previous version {previous} is not installed"));
    }
    let new = Manifest {
        active: Some(previous),
        previous: Some(active),
    };
    profile.write(MANIFEST, new.render().as_bytes())?;
    Ok(new)
}

/// Remove everything deploy installed. Returns whether anything was there.
pub fn undeploy(profile: &dyn Profile) -> Result<bool, String> {
    let record = profile.read(RECORD)?;
    let installed = record.is_some() || profile.is_dir(ROOT)? || profile.read(LOADER)?.is_some();

    profile.remove_file(LOADER)?;
    profile.remove_tree(ROOT)?;

    let record = record.map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    let mut created: Vec<&str> = record
        .as_deref()
        .unwrap_or("")
        .lines()
        .filter(|dir| LOADER_DIRS.contains(dir))
        .collect();
    created.sort_by_key(|dir| std::cmp::Reverse(dir.len()));
    for dir in created {
        profile.remove_empty_dir(dir)?;
    }
    Ok(installed)
}

/// SHA-256 of `bytes` in lower-case hex.
pub fn sha256(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Refuse a profile path inside a DCS install. The install is read-only.
pub fn refuse_install_path(path: &str) -> Result<(), String> {
    let inside = path
        .split(['/', '\\'])
        .any(|part| part.eq_ignore_ascii_case("Eagle Dynamics"));
    if inside {
        return Err(format!(
            "{path} is inside a DCS install, which is read-only; use the Saved Games profile"
        ));
    }
    Ok(())
}

/// A profile on this machine's file system.
#[derive(Debug)]
pub struct Local {
    root: PathBuf,
}

impl Local {
    /// The profile at `root`, which must exist.
    pub fn new(root: impl Into<PathBuf>) -> Result<Local, String> {
        let root = root.into();
        refuse_install_path(&root.to_string_lossy())?;
        if !root.is_dir() {
            return Err(format!("no profile folder at {}", root.display()));
        }
        Ok(Local { root })
    }

    fn path(&self, path: &str) -> PathBuf {
        let mut out = self.root.clone();
        for part in path.split('/') {
            out.push(part);
        }
        out
    }
}

fn io_error(path: &Path, e: std::io::Error) -> String {
    format!("{}: {e}", path.display())
}

impl Profile for Local {
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        let path = self.path(path);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_error(&path, e)),
        }
    }

    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        let path = self.path(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
        }
        let mut temp = path.clone().into_os_string();
        temp.push(".partial");
        let temp = PathBuf::from(temp);
        std::fs::write(&temp, bytes).map_err(|e| io_error(&temp, e))?;
        std::fs::rename(&temp, &path).map_err(|e| io_error(&path, e))
    }

    fn create_dir(&self, path: &str) -> Result<(), String> {
        let path = self.path(path);
        std::fs::create_dir_all(&path).map_err(|e| io_error(&path, e))
    }

    fn is_dir(&self, path: &str) -> Result<bool, String> {
        Ok(self.path(path).is_dir())
    }

    fn list(&self, path: &str) -> Result<Vec<String>, String> {
        let path = self.path(path);
        let entries = match std::fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_error(&path, e)),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| io_error(&path, e))?;
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        Ok(names)
    }

    fn remove_file(&self, path: &str) -> Result<(), String> {
        let path = self.path(path);
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io_error(&path, e)),
            _ => Ok(()),
        }
    }

    fn remove_tree(&self, path: &str) -> Result<(), String> {
        let path = self.path(path);
        match std::fs::remove_dir_all(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io_error(&path, e)),
            _ => Ok(()),
        }
    }

    fn remove_empty_dir(&self, path: &str) -> Result<(), String> {
        let path = self.path(path);
        if std::fs::read_dir(&path).is_ok_and(|mut entries| entries.next().is_none()) {
            std::fs::remove_dir(&path).map_err(|e| io_error(&path, e))?;
        }
        Ok(())
    }

    fn sha256(&self, path: &str) -> Result<Option<String>, String> {
        Ok(self.read(path)?.map(|bytes| sha256(&bytes)))
    }

    fn read_from_last(&self, path: &str, marker: [&str; 2]) -> Result<Option<Vec<u8>>, String> {
        Ok(self.read(path)?.and_then(|bytes| from_last(&bytes, marker)))
    }
}

/// Every path under `root` with its kind and, for a file, its size, one per
/// line, sorted. Reads no file's contents.
pub fn snapshot_local(root: &Path) -> Result<Vec<String>, String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), String> {
        for entry in std::fs::read_dir(dir).map_err(|e| io_error(dir, e))? {
            let entry = entry.map_err(|e| io_error(dir, e))?;
            let path = entry.path();
            let meta = entry.metadata().map_err(|e| io_error(&path, e))?;
            let rel = path
                .strip_prefix(root)
                .expect("under root")
                .to_string_lossy()
                .replace('\\', "/");
            if meta.is_dir() {
                out.push(format!("d {rel}"));
                walk(root, &path, out)?;
            } else {
                out.push(format!("f {} {rel}", meta.len()));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir()
                .join(format!("nanoprobe-install-{name}-{}", std::process::id()));
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

    fn payload(dll: &str) -> Payload {
        Payload {
            loader: b"-- loader\n".to_vec(),
            dll: dll.as_bytes().to_vec(),
            files: vec![
                ("lua/hook.lua".into(), b"-- hook\n".to_vec()),
                ("lua/log.lua".into(), b"-- log\n".to_vec()),
            ],
        }
    }

    /// A profile with things in it that are not the platform's.
    fn profile(scratch: &Scratch, with_hooks: bool) -> Local {
        let root = &scratch.0;
        std::fs::create_dir_all(root.join("Logs")).unwrap();
        std::fs::write(root.join("Logs/dcs.log"), "log").unwrap();
        std::fs::create_dir_all(root.join("Config")).unwrap();
        if with_hooks {
            std::fs::create_dir_all(root.join("Scripts/Hooks")).unwrap();
            std::fs::write(root.join("Scripts/Hooks/other.lua"), "-- other").unwrap();
        }
        Local::new(root).unwrap()
    }

    #[test]
    fn deploy_then_undeploy_leaves_an_identical_tree() {
        for with_hooks in [false, true] {
            let scratch = Scratch::new(if with_hooks { "hooks" } else { "bare" });
            let local = profile(&scratch, with_hooks);
            let before = snapshot_local(&scratch.0).unwrap();

            let first = deploy(&local, &payload("one")).unwrap();
            assert!(first.wrote_version && first.switched);
            let installed = snapshot_local(&scratch.0).unwrap();
            assert!(installed.contains(&format!(
                "f 3 nanoprobe/versions/{0}/nanoprobe_agent_{0}.dll",
                first.build
            )));
            assert!(installed.contains(&"d nanoprobe/logs".to_owned()));

            assert!(undeploy(&local).unwrap());
            assert_eq!(snapshot_local(&scratch.0).unwrap(), before);
            assert!(
                !undeploy(&local).unwrap(),
                "a second undeploy found something"
            );
        }
    }

    #[test]
    fn deploying_the_same_version_again_changes_nothing() {
        let scratch = Scratch::new("again");
        let local = profile(&scratch, false);
        let first = deploy(&local, &payload("one")).unwrap();
        let tree = snapshot_local(&scratch.0).unwrap();
        let manifest_text = local.read(MANIFEST).unwrap();

        let second = deploy(&local, &payload("one")).unwrap();
        assert_eq!(second.build, first.build);
        assert!(!second.wrote_version && !second.switched);
        assert_eq!(snapshot_local(&scratch.0).unwrap(), tree);
        assert_eq!(local.read(MANIFEST).unwrap(), manifest_text);
    }

    #[test]
    fn a_damaged_version_is_rewritten() {
        let scratch = Scratch::new("damaged");
        let local = profile(&scratch, false);
        let first = deploy(&local, &payload("one")).unwrap();
        local
            .write(&format!("{VERSIONS}/{}/lua/hook.lua", first.build), b"x")
            .unwrap();
        let again = deploy(&local, &payload("one")).unwrap();
        assert!(again.wrote_version);
        assert_eq!(
            local
                .read(&format!("{VERSIONS}/{}/lua/hook.lua", first.build))
                .unwrap()
                .unwrap(),
            b"-- hook\n"
        );
    }

    #[test]
    fn keeps_two_versions_and_rolls_back() {
        let scratch = Scratch::new("rollback");
        let local = profile(&scratch, false);
        assert!(
            rollback(&local).is_err(),
            "rolled back with nothing installed"
        );

        let one = deploy(&local, &payload("one")).unwrap().build;
        assert!(rollback(&local).is_err(), "rolled back with one version");
        let two = deploy(&local, &payload("two")).unwrap().build;
        let three = deploy(&local, &payload("three")).unwrap();
        assert_eq!(three.pruned, vec![one.clone()]);
        assert_eq!(local.list(VERSIONS).unwrap().len(), 2);

        let back = rollback(&local).unwrap();
        assert_eq!(back.active.as_deref(), Some(two.as_str()));
        assert_eq!(back.previous.as_deref(), Some(three.build.as_str()));
        let forward = rollback(&local).unwrap();
        assert_eq!(forward.active, Some(three.build));
    }

    #[test]
    fn the_build_follows_every_file() {
        let base = payload("one");
        let mut hook = base.clone();
        hook.files[0].1 = b"-- changed\n".to_vec();
        assert_ne!(base.build(), payload("two").build());
        assert_ne!(base.build(), hook.build());
        assert_eq!(base.build(), payload("one").build());
        assert_eq!(base.build().len(), 16);
    }

    #[test]
    fn reads_a_manifest() {
        let text = "active=abc\nprevious=def\njunk\nactive2=x\n";
        let manifest = Manifest::parse(text);
        assert_eq!(manifest.active.as_deref(), Some("abc"));
        assert_eq!(manifest.previous.as_deref(), Some("def"));
        assert_eq!(manifest.render(), "active=abc\nprevious=def\n");
        assert_eq!(Manifest::parse("active=../x\n").active, None);
    }

    #[test]
    fn refuses_a_dcs_install() {
        assert!(refuse_install_path("/c/Program Files/Eagle Dynamics/DCS World").is_err());
        assert!(refuse_install_path(r"C:\Program Files\Eagle Dynamics\DCS World").is_err());
        assert!(refuse_install_path("/c/users/u/Saved Games/DCS").is_ok());
    }
}
