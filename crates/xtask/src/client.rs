//! `client join <client> <server>`: join a server from a DCS client (E30).
//!
//! The client is a windows-ssh target running a `--dev` build, at its main
//! menu. The server is a docker-wine target whose profile names
//! `client_address`, where the client reaches it, and `client_password_file`,
//! a file on this machine that holds only the join password.
//!
//! The command reads the password file and writes its bytes to
//! `nanoprobe/dev/secret/join` in the client's profile through SSH's standard
//! input, so the password is in no command line and in no file on this
//! machine but its own. It then runs `lua/experiments/alone/e30-join.lua` in
//! the client's hook state through the eval channel, with the address filled
//! in. The snippet reads the file, removes it, and passes the password to
//! `net.start_client`. The command removes the file and its folder again on
//! every path once the write has begun, a failed write included, and checks
//! they are gone. It fails when the result does not show a join.
//!
//! What the command checks for the password: its own output and error
//! messages, the eval result (in its raw and its JSON-escaped form), and both
//! sides' `Logs/dcs.log`. A match fails the command and names the place, never
//! the value; a log it cannot read fails the check. It does not search the
//! client's other files. Every bundle redacts every target's join password
//! (`bundle::Redactor::with_secrets`).
//!
//! An interrupted command can leave the secret and a queued join request on
//! the client, which the hook would run at its next start; the next
//! `client join` removes both before anything else.
//!
//! Test tooling: built from source, never released.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use crate::install::{DEV, Profile, RUNNING};
use crate::profile::Target;
use crate::{eval, windows};

/// The secret's path in the client's profile, as the snippet reads it.
const SECRET: &str = "nanoprobe/dev/secret/join";
/// Its folder.
const SECRET_DIR: &str = "nanoprobe/dev/secret";
/// The snippet, with its address placeholder.
const SNIPPET: &str = include_str!("../../../lua/experiments/alone/e30-join.lua");
/// The placeholder in [`SNIPPET`].
const PLACEHOLDER: &str = "'<server>:<port>'";
/// How long the join may take, the snippet's own wait included.
const TIMEOUT: Duration = Duration::from_secs(100);
/// DCS's own log in a profile.
const DCS_LOG: &str = "Logs/dcs.log";

/// `client join <client> <server>`.
pub fn command(rest: &[&str]) -> Result<ExitCode, String> {
    match rest {
        ["join", client, server] => join(client, server),
        _ => Err("usage: cargo xtask client join <client-target> <server-target>".into()),
    }
}

/// `~/` at the start of `path` is the home folder.
fn expand(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => Path::new(&home).join(rest),
        _ => PathBuf::from(path),
    }
}

/// The password file's bytes. A missing, unreadable or empty file is an
/// error that names the file, never its contents. One trailing newline is
/// not part of the password.
pub(crate) fn read_password(path: &Path) -> Result<Vec<u8>, String> {
    let mut bytes = std::fs::read(path).map_err(|e| {
        format!(
            "the client password file {} cannot be read: {}",
            path.display(),
            e.kind()
        )
    })?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if bytes.is_empty() {
        return Err(format!(
            "the client password file {} is empty",
            path.display()
        ));
    }
    Ok(bytes)
}

/// A Lua string literal holding `text`, which may hold any byte.
fn lua_literal(text: &str) -> String {
    let mut out = String::from("'");
    for b in text.bytes() {
        match b {
            b'\\' => out.push_str("\\\\"),
            b'\'' => out.push_str("\\'"),
            0x20..=0x7e => out.push(char::from(b)),
            _ => out.push_str(&format!("\\{b:03}")),
        }
    }
    out.push('\'');
    out
}

/// The snippet with `address` filled in.
pub(crate) fn snippet(address: &str) -> Result<String, String> {
    if !SNIPPET.contains(PLACEHOLDER) {
        return Err("lua/experiments/alone/e30-join.lua has lost its address placeholder".into());
    }
    Ok(SNIPPET.replacen(PLACEHOLDER, &lua_literal(address), 1))
}

/// Every target's join password that can be read, for the bundle's
/// redactor. A file that cannot be read gives nothing.
pub(crate) fn passwords() -> Vec<String> {
    let Ok((_, targets)) = crate::profile::load() else {
        return Vec::new();
    };
    targets
        .values()
        .filter_map(|t| match t {
            Target::DockerWine {
                client_password_file: Some(f),
                ..
            } => read_password(&expand(f)).ok(),
            _ => None,
        })
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .collect()
}

/// Whether `haystack` holds `needle`.
pub(crate) fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// The server's address and password file, from its profile.
fn server_keys(name: &str) -> Result<(String, PathBuf), String> {
    match crate::target(name)? {
        Target::DockerWine {
            client_address,
            client_password_file,
            ..
        } => {
            let address = client_address
                .ok_or_else(|| format!("target {name:?} has no `client_address` key"))?;
            let file = client_password_file
                .ok_or_else(|| format!("target {name:?} has no `client_password_file` key"))?;
            Ok((address, expand(&file)))
        }
        _ => Err(format!("{name} is not a docker-wine target")),
    }
}

/// What `join` does on the client and the server, so its handling of the
/// password can be tested with a fake.
pub(crate) trait Join {
    /// Write the secret file. It may leave a partial file when it fails.
    fn write_secret(&self, bytes: &[u8]) -> Result<(), String>;
    /// Run the snippet in the client's hook state.
    fn eval(&self, code: &str) -> Result<serde_json::Value, String>;
    /// Remove the secret file and its folder, and whether the file is still
    /// there afterwards, found by listing its folder.
    fn remove_secret(&self) -> Result<bool, String>;
    /// The logs to check for the password, by place: `None` when one could
    /// not be read.
    fn logs(&self) -> Vec<(String, Option<Vec<u8>>)>;
}

/// The command over a real client and server.
struct Live<'a> {
    w: &'a windows::Windows,
    client: &'a str,
    server: &'a str,
}

impl Join for Live<'_> {
    fn write_secret(&self, bytes: &[u8]) -> Result<(), String> {
        self.w.write_from_stdin(SECRET, bytes)
    }

    fn eval(&self, code: &str) -> Result<serde_json::Value, String> {
        eval::run(self.w, "hook", "e30-join", code.as_bytes(), TIMEOUT)
    }

    fn remove_secret(&self) -> Result<bool, String> {
        // Every step runs, whichever fails; the first error is returned.
        let file = self.w.remove_file(SECRET);
        let tree = self.w.remove_tree(SECRET_DIR);
        let left = self.w.is_dir(SECRET_DIR);
        file?;
        tree?;
        left
    }

    fn logs(&self) -> Vec<(String, Option<Vec<u8>>)> {
        let mut logs = vec![(
            format!("{}:{DCS_LOG}", self.client),
            self.w.read(DCS_LOG).ok().flatten(),
        )];
        let server = crate::open(self.server)
            .ok()
            .and_then(|p| p.read(DCS_LOG).ok().flatten());
        logs.push((format!("{}:{DCS_LOG}", self.server), server));
        logs
    }
}

/// Whether an eval result shows a join: a multiplayer session, a player ID
/// other than the server's, and slots on offer.
fn joined(result: &serde_json::Value) -> bool {
    result["reply"]["observations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|o| o["name"] == "joined")
        .any(|o| {
            let v = &o["value"];
            v["multiplayer"] == serde_json::json!(true)
                && v["id"].as_u64().is_some_and(|id| id > 1)
                && v["blue"].as_u64().unwrap_or(0) + v["red"].as_u64().unwrap_or(0) > 0
        })
}

/// The join over `ops`: write the secret, run the snippet, remove the secret
/// on every path after the write began, and check the result and the logs
/// for the password. Returns the text to print and the failures; neither
/// holds the password.
pub(crate) fn run_join(ops: &dyn Join, code: &str, password: &[u8]) -> (Vec<String>, Vec<String>) {
    let mut out = Vec::new();
    let mut failed = Vec::new();
    let report = |failed: &mut Vec<String>, what: &str, e: &str| {
        if contains(e.as_bytes(), password) {
            failed.push(format!("{what}, and its message holds the password"));
        } else {
            failed.push(format!("{what}: {e}"));
        }
    };

    let written = ops.write_secret(password);
    let result = match &written {
        Ok(()) => Some(ops.eval(code)),
        Err(e) => {
            report(&mut failed, "could not write the secret", e);
            None
        }
    };
    // The secret goes whatever happened, a failed write included: it may
    // have left part of the file.
    match ops.remove_secret() {
        Ok(false) => {}
        Ok(true) => failed.push(format!("{SECRET} is still there")),
        Err(e) => report(&mut failed, &format!("could not remove {SECRET}"), &e),
    }

    match result {
        Some(Ok(value)) => {
            let text = serde_json::to_string_pretty(&value).unwrap_or_default();
            // JSON escapes a quote or a backslash in the password: look for
            // that form too.
            let escaped =
                serde_json::to_string(&String::from_utf8_lossy(password)).unwrap_or_default();
            let escaped = escaped.trim_matches('"').as_bytes();
            if contains(text.as_bytes(), password) || contains(text.as_bytes(), escaped) {
                failed.push("the eval result holds the password; it is not printed".into());
            } else {
                out.push(text);
            }
            if !joined(&value) {
                failed.push("the client did not join: see the result".into());
            }
        }
        Some(Err(e)) => report(&mut failed, "the eval failed", &e),
        None => {}
    }

    for (place, bytes) in ops.logs() {
        match bytes {
            Some(b) if contains(&b, password) => failed.push(format!("{place} holds the password")),
            Some(_) => out.push(format!("checked {place}: no password")),
            None => failed.push(format!("could not read {place} to check it")),
        }
    }
    (out, failed)
}

fn join(client: &str, server: &str) -> Result<ExitCode, String> {
    let (address, file) = server_keys(server)?;
    let w = match crate::target(client)? {
        Target::WindowsSsh { ssh, profile, .. } => windows::Windows::connect(&ssh, &profile)?,
        _ => return Err(format!("{client} is not a windows-ssh target")),
    };
    if w.read(RUNNING)?.is_none() {
        return Err(format!(
            "no hook runs on {client}: deploy with --dev and start DCS there first"
        ));
    }
    // A run that was interrupted may have left the secret, or a queued join
    // the hook would run at its next start: remove both first.
    w.remove_tree(SECRET_DIR)?;
    for name in w.list(&format!("{DEV}/eval/in"))? {
        if name.starts_with("e30-join") {
            w.remove_file(&format!("{DEV}/eval/in/{name}"))?;
        }
    }
    let password = read_password(&file)?;
    let code = snippet(&address)?;
    if contains(code.as_bytes(), &password) {
        return Err("the snippet would hold the password; not sending it".into());
    }
    let (out, failed) = run_join(
        &Live {
            w: &w,
            client,
            server,
        },
        &code,
        &password,
    );
    drop(password);
    for line in &out {
        println!("{line}");
    }
    if failed.is_empty() {
        Ok(ExitCode::SUCCESS)
    } else {
        for f in &failed {
            eprintln!("xtask: {f}");
        }
        Ok(ExitCode::FAILURE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A password no test input holds by chance.
    const PLANTED: &str = "np-planted-Zq7!x'\"\\pw";

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nanoprobe-client-{name}-{}",
            nanoprobe_transport::random_nonzero()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_missing_or_empty_file_fails_and_names_the_file_only() {
        let dir = scratch("missing");
        let missing = dir.join("none");
        let e = read_password(&missing).unwrap_err();
        assert!(e.contains("cannot be read") && e.contains("none"), "{e}");
        let empty = dir.join("empty");
        std::fs::write(&empty, b"\n").unwrap();
        let e = read_password(&empty).unwrap_err();
        assert!(e.contains("is empty"), "{e}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_password_is_read_whole_without_a_trailing_newline() {
        let dir = scratch("read");
        let file = dir.join("pw");
        std::fs::write(&file, format!("{PLANTED}\n")).unwrap();
        assert_eq!(read_password(&file).unwrap(), PLANTED.as_bytes());
        std::fs::write(&file, PLANTED).unwrap();
        assert_eq!(read_password(&file).unwrap(), PLANTED.as_bytes());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_snippet_holds_the_address_and_never_the_password() {
        let code = snippet("server.example:10308").unwrap();
        assert!(code.contains("local ADDRESS = 'server.example:10308'"));
        assert!(!code.contains("<server>"));
        assert!(!contains(code.as_bytes(), PLANTED.as_bytes()));
        // The snippet takes the password from the file and drops it; it
        // observes only whether the file was there.
        assert!(code.contains("os.remove(SECRET)"));
        assert!(code.contains("-- The password lives only in this block."));
        assert!(!code.contains("observe('password'"));
        assert_eq!(lua_literal("a'b\\c\n"), "'a\\'b\\\\c\\010'");
        // A byte escape followed by a digit keeps its three digits.
        assert_eq!(lua_literal("\n5"), "'\\0105'");
    }

    #[test]
    fn a_planted_password_is_found_wherever_it_lands() {
        let pw = PLANTED.as_bytes();
        let log = format!("2026 INFO net: joining with {PLANTED} now");
        assert!(contains(log.as_bytes(), pw));
        assert!(!contains(b"2026 INFO net: joining", pw));
        assert!(!contains(b"anything", b""));
        // Error messages built from a file path never carry the contents.
        let dir = scratch("messages");
        let file = dir.join("pw");
        std::fs::write(&file, b"\n").unwrap();
        let e = read_password(&file).unwrap_err();
        assert!(!contains(e.as_bytes(), pw));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_bundle_redacts_a_planted_password() {
        let text = format!("INFO ASYNCNET: password {PLANTED} in a line");
        let redacted = crate::bundle::Redactor::new(b"key", &[&text])
            .with_secrets(vec![PLANTED.into()])
            .redact(&text);
        assert!(
            !contains(redacted.as_bytes(), PLANTED.as_bytes()),
            "{redacted}"
        );
        assert!(redacted.contains("<secret>"));
    }

    /// A fake client and server for `run_join`, recording what was done.
    #[derive(Default)]
    struct Fake {
        write_fails: bool,
        eval: Option<Result<serde_json::Value, String>>,
        log: Option<Vec<u8>>,
        written: std::cell::Cell<bool>,
        removed: std::cell::Cell<bool>,
        evaluated: std::cell::Cell<bool>,
    }

    impl Join for Fake {
        fn write_secret(&self, _: &[u8]) -> Result<(), String> {
            // A failed write still leaves part of the file.
            self.written.set(true);
            if self.write_fails {
                Err("ssh: connection closed".into())
            } else {
                Ok(())
            }
        }
        fn eval(&self, _: &str) -> Result<serde_json::Value, String> {
            self.evaluated.set(true);
            self.eval.clone().unwrap_or_else(|| Err("no eval".into()))
        }
        fn remove_secret(&self) -> Result<bool, String> {
            self.removed.set(true);
            Ok(false)
        }
        fn logs(&self) -> Vec<(String, Option<Vec<u8>>)> {
            vec![("client:Logs/dcs.log".into(), self.log.clone())]
        }
    }

    fn result(multiplayer: bool, id: u64, blue: u64) -> serde_json::Value {
        serde_json::json!({ "reply": { "observations": [
            { "name": "joined", "value": { "multiplayer": multiplayer, "id": id, "blue": blue, "red": 0 } },
        ] } })
    }

    fn quiet(out: &[String], failed: &[String]) {
        for line in out.iter().chain(failed) {
            assert!(!contains(line.as_bytes(), PLANTED.as_bytes()), "{line}");
        }
    }

    #[test]
    fn a_join_that_works_prints_the_result_and_removes_the_secret() {
        let f = Fake {
            eval: Some(Ok(result(true, 2, 14))),
            log: Some(b"INFO net: joined".to_vec()),
            ..Fake::default()
        };
        let (out, failed) = run_join(&f, "code", PLANTED.as_bytes());
        assert!(failed.is_empty(), "{failed:?}");
        assert!(f.removed.get());
        assert!(out.iter().any(|l| l.contains("no password")));
        quiet(&out, &failed);
    }

    #[test]
    fn a_failed_write_still_removes_the_secret_and_runs_nothing() {
        let f = Fake {
            write_fails: true,
            log: Some(Vec::new()),
            ..Fake::default()
        };
        let (out, failed) = run_join(&f, "code", PLANTED.as_bytes());
        assert!(f.written.get() && f.removed.get());
        assert!(!f.evaluated.get());
        assert!(
            failed
                .iter()
                .any(|l| l.starts_with("could not write the secret"))
        );
        quiet(&out, &failed);
    }

    #[test]
    fn a_refused_join_or_a_failed_eval_fails_and_removes_the_secret() {
        for eval in [Ok(result(false, 0, 0)), Err("timed out".to_owned())] {
            let f = Fake {
                eval: Some(eval),
                log: Some(Vec::new()),
                ..Fake::default()
            };
            let (out, failed) = run_join(&f, "code", PLANTED.as_bytes());
            assert!(f.removed.get());
            assert!(!failed.is_empty());
            quiet(&out, &failed);
        }
    }

    #[test]
    fn a_planted_password_in_the_result_or_a_log_is_never_printed_and_fails() {
        let mut leaky = result(true, 2, 14);
        leaky["reply"]["notes"] = serde_json::json!([PLANTED]);
        let f = Fake {
            eval: Some(Ok(leaky)),
            log: Some(format!("INFO {PLANTED}").into_bytes()),
            ..Fake::default()
        };
        let (out, failed) = run_join(&f, "code", PLANTED.as_bytes());
        assert!(
            failed
                .iter()
                .any(|l| l.contains("eval result holds the password"))
        );
        assert!(
            failed
                .iter()
                .any(|l| l.contains("holds the password") && l.contains("dcs.log"))
        );
        quiet(&out, &failed);
        // An error message holding it is reported without it.
        let f = Fake {
            eval: Some(Err(format!("boom {PLANTED}"))),
            log: Some(Vec::new()),
            ..Fake::default()
        };
        let (out, failed) = run_join(&f, "code", PLANTED.as_bytes());
        quiet(&out, &failed);
    }

    #[test]
    fn an_unreadable_log_fails_the_check() {
        let f = Fake {
            eval: Some(Ok(result(true, 2, 14))),
            log: None,
            ..Fake::default()
        };
        let (_, failed) = run_join(&f, "code", PLANTED.as_bytes());
        assert!(failed.iter().any(|l| l.contains("could not read")));
    }
}
