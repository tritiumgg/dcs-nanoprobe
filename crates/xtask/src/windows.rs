//! A Saved Games profile on a Windows machine, reached over SSH.
//!
//! The SSH account's default shell is Windows PowerShell. Each operation is one
//! SSH session running one PowerShell script, sent base64-encoded so no quoting
//! layer can change it. Arguments are embedded as PowerShell string literals.
//! File output comes back through the raw standard output stream. Files are
//! sent with SFTP: a large file fed to PowerShell's standard input stalled
//! (LIVE, Windows OpenSSH with Windows PowerShell 5.1). Files are opened with read, write and delete sharing, because DCS
//! holds its logs open while it runs.
//!
//! The account is expected to be a standard user with access to the profile
//! folder only. It cannot list the profile's parent folders, so every path is
//! a full one and nothing here walks up the tree.
//!
//! DCS needs a desktop session, so it is started and stopped through two
//! scheduled tasks registered under the desktop user, which this account may
//! run.

use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::install::{Profile, refuse_install_path};
use crate::ssh;

/// Where DCS installs by default.
pub const DEFAULT_INSTALL: &str = r"C:\Program Files\Eagle Dynamics\DCS World";

/// Exit code a script uses for "no such file".
const MISSING: i32 = 3;

/// Defined at the top of every script: stop on any error, the profile root,
/// and two helpers.
const PRELUDE: &str = r#"$ErrorActionPreference = 'Stop'
function Full([string]$rel) {
    if ($rel -eq '') { return $root }
    return $root.TrimEnd('\') + '\' + ($rel -replace '/', '\')
}
function Emit([byte[]]$bytes) {
    $out = [Console]::OpenStandardOutput()
    $out.Write($bytes, 0, $bytes.Length)
    $out.Flush()
}
function EmitText([string]$text) { Emit ([Text.Encoding]::UTF8.GetBytes($text)) }
function OpenShared([string]$path) {
    return [IO.File]::Open($path, 'Open', 'Read', 'ReadWrite, Delete')
}
"#;

/// A PowerShell single-quoted string literal holding `value`.
fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// The base64 text of `bytes`, standard alphabet with padding.
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, &b)| acc | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The profile at `profile` on the Windows machine `ssh`.
#[derive(Debug, Clone)]
pub struct Windows {
    ssh: String,
    profile: String,
}

impl Windows {
    /// Connect, and check that the profile folder exists.
    pub fn connect(ssh: &str, profile: &str) -> Result<Windows, String> {
        refuse_install_path(profile)?;
        let windows = Windows {
            ssh: ssh.to_owned(),
            profile: profile.to_owned(),
        };
        let output = windows.run(
            "if (-not [IO.Directory]::Exists($root)) { exit 3 }",
            &[],
            b"",
        )?;
        match output.code {
            Some(0) => Ok(windows),
            _ => Err(format!("no profile at {profile}: {}", output.stderr)),
        }
    }

    /// Run `body` with `$root` set to the profile and `$a0`, `$a1`, ... set to
    /// `args`, feeding `input` on standard input.
    fn run(&self, body: &str, args: &[&str], input: &[u8]) -> Result<ssh::Output, String> {
        let mut script = format!("$root = {}\n", literal(&self.profile));
        for (i, arg) in args.iter().enumerate() {
            script.push_str(&format!("$a{i} = {}\n", literal(arg)));
        }
        script.push_str(PRELUDE);
        script.push_str(body);
        script.push_str("\nexit 0\n");
        let command = format!(
            "& ([ScriptBlock]::Create([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{}'))))",
            base64(script.as_bytes())
        );
        ssh::run(&self.ssh, &command, input)
    }

    /// Run a PowerShell program with `$a0`, `$a1`, ... set to `args`, and with
    /// the helpers `EmitText` and `Emit` defined.
    pub fn powershell(&self, program: &str, args: &[&str]) -> Result<ssh::Output, String> {
        self.run(program, args, b"")
    }

    /// The profile folder's Windows path.
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// Run `body` on `path` (as `$p`, the full path) and return its standard
    /// output; any exit but 0 is an error.
    fn ok(&self, body: &str, path: &str, input: &[u8]) -> Result<Vec<u8>, String> {
        let body = format!("$p = Full $a0\n{body}");
        let output = self.run(&body, &[path], input)?;
        match output.code {
            Some(0) => Ok(output.stdout),
            code => Err(format!("{path}: exit {code:?}: {}", output.stderr)),
        }
    }

    /// As [`Windows::ok`], with exit 3 meaning "no such file".
    fn maybe(&self, body: &str, path: &str) -> Result<Option<Vec<u8>>, String> {
        let body = format!("$p = Full $a0\n{body}");
        let output = self.run(&body, &[path], b"")?;
        match output.code {
            Some(0) => Ok(Some(output.stdout)),
            Some(MISSING) => Ok(None),
            code => Err(format!("{path}: exit {code:?}: {}", output.stderr)),
        }
    }

    /// Send `bytes` to the Windows path `full` with SFTP, through a local
    /// temporary file.
    fn send(&self, full: &str, bytes: &[u8]) -> Result<(), String> {
        if full.contains('"') {
            return Err(format!("{full}: a path with a double quote cannot be sent"));
        }
        static SENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SENT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let local = std::env::temp_dir().join(format!("nanoprobe-send-{}-{n}", std::process::id()));
        std::fs::write(&local, bytes).map_err(|e| format!("{}: {e}", local.display()))?;
        // SFTP names a Windows path as /C:/folder/file.
        let remote = format!("/{}", full.replace('\\', "/"));
        let batch = format!("put \"{}\" \"{remote}\"\n", local.display());
        let result = std::process::Command::new("sftp")
            .args([
                "-q",
                "-b",
                "-",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=15",
            ])
            .args([
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=4",
            ])
            .arg(&self.ssh)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write as _;
                child
                    .stdin
                    .take()
                    .expect("stdin is piped")
                    .write_all(batch.as_bytes())?;
                child.wait_with_output()
            });
        let _ = std::fs::remove_file(&local);
        let output = result.map_err(|e| format!("could not run sftp: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "sftp to {full} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }

    /// Run the scheduled task `task` with `schtasks /run`.
    pub fn run_task(&self, task: &str) -> Result<(), String> {
        let output = self.run(
            "schtasks /run /tn $a0 | Out-Null\nif ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }",
            &[task],
            b"",
        )?;
        match output.code {
            Some(0) => Ok(()),
            code => Err(format!(
                "schtasks /run {task}: exit {code:?}: {}",
                output.stderr
            )),
        }
    }

    /// How many `DCS.exe` processes run on the machine, in any session.
    pub fn dcs_processes(&self) -> Result<usize, String> {
        let output = self.run(
            "EmitText ([string]@(Get-Process -Name DCS -ErrorAction SilentlyContinue).Count)",
            &[],
            b"",
        )?;
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .map_err(|_| format!("could not count DCS processes: {}", output.stderr))
    }

    /// Stop DCS with `stop`, wait until no `DCS.exe` runs, then start it with
    /// `start`.
    pub fn restart_tasks(&self, start: &str, stop: &str) -> Result<(), String> {
        if self.dcs_processes()? > 0 {
            self.run_task(stop)?;
            let deadline = Instant::now() + Duration::from_secs(60);
            while self.dcs_processes()? > 0 {
                if Instant::now() > deadline {
                    return Err("DCS still runs 60 s after the stop task".into());
                }
                sleep(Duration::from_secs(2));
            }
        }
        self.run_task(start)
    }

    /// Add `block` to the end of `Config/autoexec.cfg`, between marker lines,
    /// after saving a byte-exact copy as `autoexec.cfg.pre-nanoprobe`; or with
    /// None put that copy back. The file's contents are never read out. `on`
    /// refuses when a copy is already there. When there was no file, the copy
    /// is an empty `autoexec.cfg.pre-nanoprobe-absent` marker, and `off` deletes
    /// the file.
    pub fn autoexec_block(&self, block: Option<&str>) -> Result<String, String> {
        let body = match block {
            Some(_) => {
                r#"$p = Full 'Config/autoexec.cfg'
$copy = $p + '.pre-nanoprobe'
$absent = $p + '.pre-nanoprobe-absent'
if ([IO.File]::Exists($copy) -or [IO.File]::Exists($absent)) { EmitText 'a saved copy is already there; run gates off first'; exit 4 }
[void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($p))
if ([IO.File]::Exists($p)) {
    [IO.File]::Copy($p, $copy)
    $bytes = [IO.File]::ReadAllBytes($p)
    $sep = $(if ($bytes.Length -gt 0 -and $bytes[$bytes.Length - 1] -ne 10) { "`r`n" } else { '' })
    $created = 0
} else {
    [IO.File]::WriteAllText($absent, '')
    $sep = ''
    $created = 1
}
[IO.File]::AppendAllText($p, $sep + "-- nanoprobe-gates begin`r`n" + $a0.Replace("`n", "`r`n") + "-- nanoprobe-gates end`r`n")
EmitText "added (created=$created)""#
            }
            None => {
                r#"$p = Full 'Config/autoexec.cfg'
$copy = $p + '.pre-nanoprobe'
$absent = $p + '.pre-nanoprobe-absent'
if ([IO.File]::Exists($copy)) {
    [IO.File]::Copy($copy, $p, $true)
    [IO.File]::Delete($copy)
    EmitText 'restored the saved copy'
} elseif ([IO.File]::Exists($absent)) {
    if ([IO.File]::Exists($p)) { [IO.File]::Delete($p) }
    [IO.File]::Delete($absent)
    EmitText 'removed the file, which did not exist before'
} else {
    EmitText 'no saved copy; nothing to restore'
}"#
            }
        };
        let output = self.run(body, &[block.unwrap_or("")], b"")?;
        let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        match output.code {
            Some(0) => Ok(text),
            code => Err(format!(
                "autoexec.cfg: exit {code:?}: {text} {}",
                output.stderr
            )),
        }
    }

    /// The facts `target check` prints: Windows' version, each install's
    /// version and branch, and whether the profile and both tasks exist, as
    /// `key=value` lines.
    pub fn check(&self, install: &str, tasks: &[&str]) -> Result<String, String> {
        let body = r#"$v = [Environment]::OSVersion.VersionString
EmitText "os=$v`n"
if ([IO.Directory]::Exists($root)) { EmitText "profile=$root`n" }
$cfg = $a0.TrimEnd('\') + '\autoupdate.cfg'
if ([IO.File]::Exists($cfg)) {
    EmitText "install=$a0`n"
    $text = [IO.File]::ReadAllText($cfg)
    $m = [regex]::Match($text, '"version":\s*"([^"]*)"')
    if ($m.Success) { EmitText ("version=" + $m.Groups[1].Value + "`n") }
    $m = [regex]::Match($text, '"branch":\s*"([^"]*)"')
    if ($m.Success) { EmitText ("branch=" + $m.Groups[1].Value + "`n") }
}
foreach ($t in @($a1, $a2)) {
    schtasks /query /tn $t 2>$null | Out-Null
    EmitText ("task=" + $t + " " + ($(if ($LASTEXITCODE -eq 0) { 'present' } else { 'MISSING' })) + "`n")
}"#;
        let mut args = vec![install];
        args.extend_from_slice(tasks);
        let output = self.run(body, &args, b"")?;
        match output.code {
            Some(0) => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
            code => Err(format!("target check: exit {code:?}: {}", output.stderr)),
        }
    }
}

impl Profile for Windows {
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        self.maybe(
            r#"if (-not [IO.File]::Exists($p)) { exit 3 }
$f = OpenShared $p
$out = [Console]::OpenStandardOutput()
$f.CopyTo($out)
$out.Flush()
$f.Close()"#,
            path,
        )
    }

    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        // Make the folder, send the file under a temporary name with SFTP,
        // then put it in place in one rename.
        let full = self.ok(
            "[void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($p))\nEmitText $p",
            path,
            b"",
        )?;
        let full = String::from_utf8_lossy(&full).trim().to_owned();
        self.send(&format!("{full}.partial"), bytes)?;
        let body = r#"$tmp = $p + '.partial'
if ([IO.File]::Exists($p)) { [IO.File]::Replace($tmp, $p, $null) } else { [IO.File]::Move($tmp, $p) }"#;
        self.ok(body, path, b"").map(drop)
    }

    fn create_dir(&self, path: &str) -> Result<(), String> {
        self.ok("[void][IO.Directory]::CreateDirectory($p)", path, b"")
            .map(drop)
    }

    fn is_dir(&self, path: &str) -> Result<bool, String> {
        Ok(self
            .maybe("if (-not [IO.Directory]::Exists($p)) { exit 3 }", path)?
            .is_some())
    }

    fn list(&self, path: &str) -> Result<Vec<String>, String> {
        let out = self.ok(
            r#"if (-not [IO.Directory]::Exists($p)) { exit 0 }
$names = [IO.Directory]::GetFileSystemEntries($p) | ForEach-Object { [IO.Path]::GetFileName($_) }
EmitText (($names -join "`n") + "`n")"#,
            path,
            b"",
        )?;
        let mut names: Vec<String> = String::from_utf8_lossy(&out)
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect();
        names.sort();
        Ok(names)
    }

    fn remove_file(&self, path: &str) -> Result<(), String> {
        self.ok(
            "if ([IO.File]::Exists($p)) { [IO.File]::Delete($p) }",
            path,
            b"",
        )
        .map(drop)
    }

    fn remove_tree(&self, path: &str) -> Result<(), String> {
        self.ok(
            "if ([IO.Directory]::Exists($p)) { [IO.Directory]::Delete($p, $true) }",
            path,
            b"",
        )
        .map(drop)
    }

    fn remove_empty_dir(&self, path: &str) -> Result<(), String> {
        self.ok(
            r#"if ([IO.Directory]::Exists($p) -and @([IO.Directory]::GetFileSystemEntries($p)).Count -eq 0) {
    [IO.Directory]::Delete($p)
}"#,
            path,
            b"",
        )
        .map(drop)
    }

    fn sha256(&self, path: &str) -> Result<Option<String>, String> {
        let out = self.maybe(
            r#"if (-not [IO.File]::Exists($p)) { exit 3 }
$f = OpenShared $p
$h = [Security.Cryptography.SHA256]::Create().ComputeHash($f)
$f.Close()
EmitText (($h | ForEach-Object { $_.ToString('x2') }) -join '')"#,
            path,
        )?;
        Ok(out.map(|bytes| String::from_utf8_lossy(&bytes).trim().to_owned()))
    }

    fn read_from_last(&self, path: &str, marker: [&str; 2]) -> Result<Option<Vec<u8>>, String> {
        // The text is cut on the target, so earlier lines never leave it.
        let body = r#"$p = Full $a0
if (-not [IO.File]::Exists($p)) { exit 3 }
$f = OpenShared $p
$m = New-Object IO.MemoryStream
$f.CopyTo($m)
$f.Close()
$text = [Text.Encoding]::UTF8.GetString($m.ToArray())
$start = -1
$offset = 0
foreach ($line in [regex]::Split($text, '(?<=\n)')) {
    if ($line.Contains($a1) -and $line.Contains($a2)) { $start = $offset }
    $offset += $line.Length
}
if ($start -lt 0) { exit 3 }
EmitText $text.Substring($start)"#;
        let output = self.run(body, &[path, marker[0], marker[1]], b"")?;
        match output.code {
            Some(0) => Ok(Some(output.stdout)),
            Some(MISSING) => Ok(None),
            code => Err(format!("{path}: exit {code:?}: {}", output.stderr)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet_with_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xff, 0x00, 0xfe]), "/wD+");
    }

    #[test]
    fn a_literal_doubles_its_single_quotes() {
        assert_eq!(literal("C:\\Users\\a'b"), "'C:\\Users\\a''b'");
    }
}
