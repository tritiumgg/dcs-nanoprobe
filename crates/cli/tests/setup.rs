//! `nanoprobe install`, `doctor` and `uninstall` through the command line,
//! with a made-up payload folder laid out as a release's, into a made-up Wine
//! prefix.

use std::path::{Path, PathBuf};

use nanoprobe_cli::doctor::RELEASE_LOADER;

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("nanoprobe-setup-{name}-{}", std::process::id()));
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

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// Every path under `root` with its kind, its mode on Unix, and a file's
/// bytes.
fn tree(root: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(root).unwrap().display().to_string();
            let meta = std::fs::metadata(&path).unwrap();
            #[cfg(unix)]
            let mode = std::os::unix::fs::PermissionsExt::mode(&meta.permissions());
            #[cfg(not(unix))]
            let mode = u32::from(meta.permissions().readonly());
            if meta.is_dir() {
                out.push(format!("d {mode:o} {rel}"));
                walk(root, &path, out);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                out.push(format!("f {mode:o} {rel} {bytes:?}"));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// A payload folder as `tools/stage-release.sh` lays it out.
fn payload(dir: &Path) {
    write(&dir.join("nanoprobe-hook.lua"), RELEASE_LOADER);
    write(&dir.join("nanoprobe_agent.dll"), b"MZ fake agent");
    for name in ["hook.lua", "log.lua", "shim.lua"] {
        write(
            &dir.join("lua").join(name),
            format!("-- {name}\n").as_bytes(),
        );
    }
}

fn run(args: &[&str]) -> (i32, String, String) {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = nanoprobe_cli::run(&args, &mut out, &mut err);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn installs_checks_and_uninstalls_through_the_command_line() {
    let scratch = Scratch::new("cli");
    let prefix = scratch.0.join("prefix");
    let profile = prefix.join("drive_c/users/abc/Saved Games/DCS.dcs_serverrelease");
    write(
        &profile.join("Config/autoexec.cfg"),
        b"options.example = 1\n",
    );
    write(&profile.join("Logs/dcs.log"), b"made-up line\n");
    let dir = scratch.0.join("payload");
    payload(&dir);
    let prefix_arg = prefix.to_str().unwrap();
    let before = tree(&prefix);

    let (code, out, err) = run(&[
        "install",
        "--wine-prefix",
        prefix_arg,
        "--payload",
        dir.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("installed version"), "{out}");
    assert!(out.contains("added the nanoprobe block"), "{out}");

    let (code, out, err) = run(&["doctor", "--wine-prefix", prefix_arg]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("from before the install"), "{out}");

    let (code, out, err) = run(&["uninstall", "--wine-prefix", prefix_arg]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("removed nanoprobe"), "{out}");
    assert_eq!(tree(&prefix), before);

    let (code, out, _) = run(&["uninstall", "--wine-prefix", prefix_arg]);
    assert_eq!(code, 0);
    assert!(out.contains("was not installed"), "{out}");
}

#[test]
fn install_refuses_a_missing_payload_and_two_profiles() {
    let scratch = Scratch::new("refuse");
    let prefix = scratch.0.join("prefix");
    let saved = prefix.join("drive_c/users/abc/Saved Games");
    std::fs::create_dir_all(saved.join("DCS")).unwrap();
    let dir = scratch.0.join("payload");
    payload(&dir);
    std::fs::remove_file(dir.join("lua/shim.lua")).unwrap();
    let prefix_arg = prefix.to_str().unwrap();
    let before = tree(&prefix);

    let (code, _, err) = run(&[
        "install",
        "--wine-prefix",
        prefix_arg,
        "--payload",
        dir.to_str().unwrap(),
    ]);
    assert_eq!(code, 1);
    assert!(err.contains("shim.lua"), "{err}");
    assert!(err.contains("--payload"), "{err}");

    payload(&dir);
    std::fs::create_dir_all(saved.join("DCS.openbeta")).unwrap();
    let (code, _, err) = run(&[
        "install",
        "--wine-prefix",
        prefix_arg,
        "--payload",
        dir.to_str().unwrap(),
    ]);
    assert_eq!(code, 1);
    assert!(err.contains("more than one DCS profile"), "{err}");
    std::fs::remove_dir(saved.join("DCS.openbeta")).unwrap();
    assert_eq!(tree(&prefix), before);
}
