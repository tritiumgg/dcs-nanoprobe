//! Running commands on a target over SSH.
//!
//! BatchMode keeps SSH from asking for a password: keys come from the user's
//! own agent and config. xtask stores no credential and asks for none.

use std::io::Write;
use std::process::{Command, Stdio};

/// What a remote command left.
#[derive(Debug)]
pub struct Output {
    /// The exit code, or None when a signal ended it.
    pub code: Option<i32>,
    /// Standard output.
    pub stdout: Vec<u8>,
    /// Standard error, as text.
    pub stderr: String,
}

/// Run `remote` on `destination`, feeding `input` on stdin. SSH joins its
/// arguments into one string for the remote shell, so `remote` must already
/// be quoted. Exit code 255 is SSH's own failure, and is an error here.
pub fn run(destination: &str, remote: &str, input: &[u8]) -> Result<Output, String> {
    let mut command = Command::new("ssh");
    command.args(["-o", "BatchMode=yes"]);
    // One deploy makes dozens of calls. Sharing one connection for a minute
    // saves a handshake on each. Windows OpenSSH has no connection sharing.
    // The socket path stays short, because a Unix socket path is limited to
    // about 100 bytes.
    if cfg!(unix) {
        command.args([
            "-o",
            "ControlMaster=auto",
            "-o",
            "ControlPath=/tmp/nanoprobe-ssh-%C",
            "-o",
            "ControlPersist=60",
        ]);
    }
    let mut child = command
        .args(["--", destination, remote])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run ssh: {e}"))?;

    let mut stdin = child.stdin.take().expect("stdin is piped");
    // A command that exits without reading stdin closes the pipe; its exit
    // code says what happened, so a failed write is not an error by itself.
    let _ = stdin.write_all(input);
    drop(stdin);

    let output = child
        .wait_with_output()
        .map_err(|e| format!("ssh failed: {e}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if output.status.code() == Some(255) {
        return Err(format!("ssh {destination} failed: {stderr}"));
    }
    Ok(Output {
        code: output.status.code(),
        stdout: output.stdout,
        stderr,
    })
}

/// Run `script` on `destination` through `remote`, feeding the script on
/// stdin, and return its standard output. Any non-zero exit is an error.
pub fn script(destination: &str, remote: &str, script: &str) -> Result<String, String> {
    let output = run(destination, remote, script.as_bytes())?;
    if output.code != Some(0) {
        return Err(format!(
            "ssh {destination} failed ({:?}): {}",
            output.code, output.stderr
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Quote `value` as one word for a POSIX shell.
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_a_single_quote() {
        assert_eq!(quote("a'b c"), r"'a'\''b c'");
    }
}
