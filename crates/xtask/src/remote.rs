//! A Saved Games profile inside a Docker container, reached over SSH.
//!
//! Each operation is one SSH session running `docker exec` with a short POSIX
//! script. Files are written as the profile folder's owner, so DCS can read
//! and replace them, and land through a rename, so a reader never sees half a
//! file. Nothing here reads a file outside the paths it is given.

use crate::install::{Profile, refuse_install_path};
use crate::ssh::{self, quote};

/// The profile at `profile` in `container` on `ssh`.
#[derive(Debug)]
pub struct Remote {
    ssh: String,
    container: String,
    profile: String,
    owner: String,
}

/// Exit code a script uses for "no such file".
const MISSING: i32 = 3;

impl Remote {
    /// Connect, and read the profile folder's owner. Fails when the folder
    /// does not exist.
    pub fn connect(ssh: &str, container: &str, profile: &str) -> Result<Remote, String> {
        refuse_install_path(profile)?;
        let output = ssh::run(
            ssh,
            &format!(
                "docker exec {} stat -c %u:%g -- {}",
                quote(container),
                quote(profile)
            ),
            b"",
        )?;
        if output.code != Some(0) {
            return Err(format!("no profile at {profile}: {}", output.stderr));
        }
        let owner = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if owner.is_empty() || !owner.bytes().all(|b| b.is_ascii_digit() || b == b':') {
            return Err(format!("could not read the owner of {profile}: {owner:?}"));
        }
        Ok(Remote {
            ssh: ssh.to_owned(),
            container: container.to_owned(),
            profile: profile.to_owned(),
            owner,
        })
    }

    fn full(&self, path: &str) -> String {
        format!("{}/{path}", self.profile.trim_end_matches('/'))
    }

    /// How many players the current `dcs.log` shows connected. A player is a
    /// client with a name: `added client[N] name=...` (LOG), and leaves with
    /// `disconnecting client[N]` or `remove: client[N]` (LOG). Unnamed
    /// connection attempts are not players. Prints no name.
    pub fn players(&self) -> Result<usize, String> {
        let script = r#"[ -f "$1" ] || { echo 0; exit 0; }
awk '
    function id(   s) { match($0, /client\[[0-9]+\]/); return substr($0, RSTART + 7, RLENGTH - 8) }
    /added client\[[0-9]+\] name=/ { on[id()] = 1; next }
    /disconnecting client\[[0-9]+\]/ || /remove: client\[[0-9]+\]/ { delete on[id()] }
    END { n = 0; for (k in on) n++; print n }
' "$1""#;
        let output = self.exec(script, "Logs/dcs.log", b"")?;
        let text = String::from_utf8_lossy(&output.stdout);
        if output.code != Some(0) {
            return Err(format!("could not count players: {}", output.stderr));
        }
        text.trim()
            .parse()
            .map_err(|_| format!("could not count players: {text:?}"))
    }

    /// Run a Python 3 program in the container as the profile's owner, with
    /// `args`, and return its exit code, standard output and standard error.
    pub fn python(&self, program: &str, args: &[&str]) -> Result<ssh::Output, String> {
        let mut remote = format!(
            "docker exec -i -u {} {} python3 -",
            quote(&self.owner),
            quote(&self.container),
        );
        for arg in args {
            remote.push(' ');
            remote.push_str(&quote(arg));
        }
        ssh::run(&self.ssh, &remote, program.as_bytes())
    }

    /// Add `block` to the end of `Config/autoexec.cfg`, between marker lines,
    /// after saving a byte-exact copy as `autoexec.cfg.pre-nanoprobe`; or with
    /// None put that copy back. The file's contents are never read out. `on`
    /// refuses when a copy is already there. When there was no file, the copy
    /// is an empty `autoexec.cfg.pre-nanoprobe-absent` marker, and `off` deletes
    /// the file.
    pub fn autoexec_block(&self, block: Option<&str>) -> Result<String, String> {
        let script = match block {
            Some(_) => {
                r#"f=$1
if [ -e "$f.pre-nanoprobe" ] || [ -e "$f.pre-nanoprobe-absent" ]; then echo 'a saved copy is already there; run gates off first'; exit 4; fi
mkdir -p "$(dirname "$f")"
if [ -f "$f" ]; then
    cp -p "$f" "$f.pre-nanoprobe"
    created=0
    [ -s "$f" ] && [ -n "$(tail -c 1 "$f")" ] && printf '\n' >> "$f"
else
    : > "$f.pre-nanoprobe-absent"
    created=1
fi
{ printf -- '-- nanoprobe-gates begin\n'; cat; printf -- '-- nanoprobe-gates end\n'; } >> "$f"
echo "added (created=$created)""#
            }
            None => {
                r#"f=$1
if [ -f "$f.pre-nanoprobe" ]; then
    cat "$f.pre-nanoprobe" > "$f" && rm -f "$f.pre-nanoprobe"
    echo 'restored the saved copy'
elif [ -e "$f.pre-nanoprobe-absent" ]; then
    rm -f "$f" "$f.pre-nanoprobe-absent"
    echo 'removed the file, which did not exist before'
else
    echo 'no saved copy; nothing to restore'
fi"#
            }
        };
        let output = self.exec(
            script,
            "Config/autoexec.cfg",
            block.unwrap_or("").as_bytes(),
        )?;
        let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        match output.code {
            Some(0) => Ok(text),
            code => Err(format!(
                "autoexec.cfg: exit {code:?}: {text} {}",
                output.stderr
            )),
        }
    }

    /// A path under the profile, as the container sees it.
    pub fn full_path(&self, path: &str) -> String {
        self.full(path)
    }

    /// Run a POSIX `script` in the container as the profile's owner, with
    /// `args` as `$1`, `$2`, ..., feeding `input`.
    pub fn shell(&self, script: &str, args: &[&str], input: &[u8]) -> Result<ssh::Output, String> {
        self.exec_args(script, args, input)
    }

    /// Run `script` as the profile's owner with `$1` set to `path` under the
    /// profile, feeding `input`.
    fn exec(&self, script: &str, path: &str, input: &[u8]) -> Result<ssh::Output, String> {
        self.exec_args(script, &[&self.full(path)], input)
    }

    /// Run `script` as the profile's owner with `args` as `$1`, `$2`, ...
    fn exec_args(&self, script: &str, args: &[&str], input: &[u8]) -> Result<ssh::Output, String> {
        // The remote shell on the host parses this line once; `sh -c` in the
        // container parses the script. Each argument is quoted for the host.
        let mut remote = format!(
            "docker exec -i -u {} {} sh -c {} sh",
            quote(&self.owner),
            quote(&self.container),
            quote(script),
        );
        for arg in args {
            remote.push(' ');
            remote.push_str(&quote(arg));
        }
        ssh::run(&self.ssh, &remote, input)
    }

    fn ok(&self, script: &str, path: &str, input: &[u8]) -> Result<Vec<u8>, String> {
        let output = self.exec(script, path, input)?;
        match output.code {
            Some(0) => Ok(output.stdout),
            code => Err(format!("{path}: exit {code:?}: {}", output.stderr)),
        }
    }

    fn maybe(&self, script: &str, path: &str) -> Result<Option<Vec<u8>>, String> {
        let output = self.exec(script, path, b"")?;
        match output.code {
            Some(0) => Ok(Some(output.stdout)),
            Some(MISSING) => Ok(None),
            code => Err(format!("{path}: exit {code:?}: {}", output.stderr)),
        }
    }

    /// Every path under the profile with its kind and, for a file, its size,
    /// sorted, as [`crate::install::snapshot_local`] lists them. Reads no
    /// file's contents.
    pub fn snapshot(&self) -> Result<Vec<String>, String> {
        let script = r#"cd "$1" && find . -mindepth 1 \( -type d -printf 'd %P\n' \) -o \( -printf 'f %s %P\n' \)"#;
        let output = self.exec(script, "", b"")?;
        if output.code != Some(0) {
            return Err(format!("snapshot failed: {}", output.stderr));
        }
        let mut lines: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_owned)
            .collect();
        lines.sort();
        Ok(lines)
    }
}

impl Profile for Remote {
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        self.maybe(r#"[ -f "$1" ] || exit 3; cat -- "$1""#, path)
    }

    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        let script = r#"set -e; mkdir -p -- "$(dirname -- "$1")"; cat > "$1.partial"; mv -f -- "$1.partial" "$1""#;
        self.ok(script, path, bytes).map(drop)
    }

    fn write_private(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        let script = r#"set -e; mkdir -p -- "$(dirname -- "$1")"; umask 077; cat > "$1.partial"; chmod 600 "$1.partial"; mv -f -- "$1.partial" "$1""#;
        self.ok(script, path, bytes).map(drop)
    }

    fn create_dir(&self, path: &str) -> Result<(), String> {
        self.ok(r#"mkdir -p -- "$1""#, path, b"").map(drop)
    }

    fn is_dir(&self, path: &str) -> Result<bool, String> {
        Ok(self.maybe(r#"[ -d "$1" ] || exit 3"#, path)?.is_some())
    }

    fn list(&self, path: &str) -> Result<Vec<String>, String> {
        let out = self.ok(r#"[ -d "$1" ] || exit 0; ls -A -- "$1""#, path, b"")?;
        let mut names: Vec<String> = String::from_utf8_lossy(&out)
            .lines()
            .map(str::to_owned)
            .collect();
        names.sort();
        Ok(names)
    }

    fn remove_file(&self, path: &str) -> Result<(), String> {
        self.ok(r#"rm -f -- "$1""#, path, b"").map(drop)
    }

    fn remove_tree(&self, path: &str) -> Result<(), String> {
        self.ok(r#"rm -rf -- "$1""#, path, b"").map(drop)
    }

    fn remove_empty_dir(&self, path: &str) -> Result<(), String> {
        self.ok(r#"rmdir -- "$1" 2>/dev/null || true"#, path, b"")
            .map(drop)
    }

    fn read_from_last(&self, path: &str, marker: [&str; 2]) -> Result<Option<Vec<u8>>, String> {
        // awk keeps the text from the latest marker line; exit 3 when the
        // file or the marker is missing.
        let script = r#"[ -f "$1" ] || exit 3
awk -v a="$2" -v b="$3" '
    index($0, a) && index($0, b) { buf = ""; found = 1 }
    found { buf = buf $0 "\n" }
    END { if (!found) exit 3; printf "%s", buf }
' "$1""#;
        let output = self.exec_args(script, &[&self.full(path), marker[0], marker[1]], b"")?;
        match output.code {
            Some(0) => Ok(Some(output.stdout)),
            Some(MISSING) => Ok(None),
            code => Err(format!("{path}: exit {code:?}: {}", output.stderr)),
        }
    }

    fn sha256(&self, path: &str) -> Result<Option<String>, String> {
        let out = self.maybe(r#"[ -f "$1" ] || exit 3; sha256sum -- "$1""#, path)?;
        Ok(out.map(|bytes| {
            String::from_utf8_lossy(&bytes)
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_owned()
        }))
    }
}
